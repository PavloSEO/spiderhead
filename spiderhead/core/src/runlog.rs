//! One append-only journal of every command and tool call the toolkit runs.
//!
//! The journal answers "what did the run actually do" (tool, arguments, duration, outcome) and
//! makes repeated work visible through a fingerprint of each call. Reuse of a stored answer is
//! opt-in per tool (`SEOHEAD_REUSE_POLICY`), defaults to never, and is never silent: a reused
//! answer carries `reused: true` in the returned value and in the journal entry that answered it.
//!
//! Format is JSONL, one line per call, appended after the call completes so an interrupted
//! process cannot corrupt earlier entries. The default path is `~/.config/seohead/runs.jsonl`;
//! `SEOHEAD_RUN_LOG` overrides it, and `off` disables it.
//!
//! Behaviour notes:
//! - Arguments are `serde_json` values, so every argument is JSON already.
//! - A [`Journal`] is built once from a [`Host`] and carries its own path, ceiling, reuse policy
//!   and interface name. Callers that need a changed policy build a new `Journal`.
//! - A failing call's error text is the error's `Display`, with no exception-class prefix.
//!   A sensitive call records `sensitive operation failed` alone.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::common::python_json;
use crate::envvar::read_env;
use crate::host::Host;
use crate::timeutil::{stamp_epoch, Format};

/// Call arguments as the journal sees them: a JSON object, empty when the call had none.
pub type Args = Map<String, Value>;
/// Result facts a body adds to its own journal entry.
pub type Facts = Map<String, Value>;

/// Default journal location, expanded against `HOME`.
pub const DEFAULT_PATH: &str = "~/.config/seohead/runs.jsonl";
/// Environment variable naming the journal file, or `off`.
pub const LOG_ENV: &str = "SEOHEAD_RUN_LOG";
/// Environment variable overriding the size ceiling of the live journal.
pub const MAX_BYTES_ENV: &str = "SEOHEAD_RUN_LOG_MAX_BYTES";
/// Default size ceiling of the live journal (5 MiB).
pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Environment variable holding the per-tool reuse policy (a JSON object of max ages).
pub const REUSE_POLICY_ENV: &str = "SEOHEAD_REUSE_POLICY";
/// Argument-name fragments whose values never reach the journal (matched lowercased).
pub const SECRET_HINTS: [&str; 7] = [
    "token",
    "key",
    "secret",
    "password",
    "passwd",
    "auth",
    "credential",
];
/// Argument names redacted outright, whatever their value.
const PROXY_NAMES: [&str; 3] = ["http.proxy", "proxy", "proxy_url"];
/// Whole argument names that hold credentials without containing a secret-hint token;
/// they are listed so they stay redacted (`Authorization` contains no hint token).
const SECRET_NAMES: [&str; 8] = [
    "authorization",
    "proxy-authorization",
    "www-authenticate",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "apikey",
];
/// Longest string kept verbatim; longer values are cut and suffixed with an ellipsis.
pub const MAX_VALUE_CHARS: usize = 300;
/// Largest encoded result stored for reuse, in characters.
pub const MAX_REUSABLE_RESULT_BYTES: usize = 20_000;
/// Arguments an interface adds for display only; they do not change a call's identity.
const DISPLAY_ONLY_ARGUMENTS: [&str; 1] = ["progress"];
/// Elements of a list argument kept in the journal before summarising the rest.
const MAX_LIST_ITEMS: usize = 10;
/// Entries scanned when looking for a reusable answer.
const REUSE_SCAN_LIMIT: usize = 2000;
/// Result keys copied from a body's dict result into its journal entry.
const COPIED_FACT_KEYS: [&str; 4] = ["urls_collected", "out_dir", "scan", "count"];
const REDACTED: &str = "[redacted]";

/// Current wall-clock time as seconds since the epoch.
pub fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Shorten a value for the journal without changing its meaning.
pub fn redact(value: &Value) -> Value {
    match value {
        Value::String(text) => {
            if text.chars().count() <= MAX_VALUE_CHARS {
                Value::String(text.clone())
            } else {
                let cut: String = text.chars().take(MAX_VALUE_CHARS).collect();
                Value::String(format!("{cut}…"))
            }
        }
        Value::Array(items) => {
            let mut shown: Vec<Value> = items.iter().take(MAX_LIST_ITEMS).map(redact).collect();
            if items.len() > MAX_LIST_ITEMS {
                shown.push(Value::String(format!(
                    "…+{} more",
                    items.len() - MAX_LIST_ITEMS
                )));
            }
            Value::Array(shown)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| {
                    let shown = if is_secret_name(key) {
                        Value::String(REDACTED.into())
                    } else {
                        redact(item)
                    };
                    (key.clone(), shown)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Lowercase word tokens of an argument name: split on non-alphanumerics and on
/// camel/digit boundaries, so `apiKey` yields `["api", "key"]` while `keyword` and
/// `author` stay single tokens, so substring matches do not over-match them.
fn name_tokens(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
            prev = None;
            continue;
        }
        let boundary = matches!(
            prev,
            Some(p) if (p.is_lowercase() && c.is_uppercase())
                || (p.is_alphabetic() != c.is_alphabetic())
        );
        if boundary && !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
        token.extend(c.to_lowercase());
        prev = Some(c);
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

/// Whether an argument name denotes a credential: an exact credential name, or a token equal
/// to a hint (`api_key`, `x-auth-token`, `oauth`), or a token ending in a multi-letter hint
/// (`mysecret`). A bare plural (`credentials`, `keys`) still matches its stem.
fn is_secret_name(name: &str) -> bool {
    let lowered = name.to_lowercase();
    if PROXY_NAMES.contains(&lowered.as_str()) || SECRET_NAMES.contains(&lowered.as_str()) {
        return true;
    }
    name_tokens(name).iter().any(|token| {
        let bare = token.strip_suffix('s').unwrap_or(token.as_str());
        SECRET_HINTS
            .iter()
            .any(|hint| token == hint || bare == *hint)
            || (token.len() >= 5
                && SECRET_HINTS
                    .iter()
                    .filter(|hint| hint.len() >= 4)
                    .any(|hint| token.ends_with(hint)))
    })
}

/// Drop credentials and shorten the rest. A journal that leaks an API key is worse than none.
///
/// `is_secret_name` checks nested object keys too, at any depth.
pub fn safe_arguments(arguments: &Args) -> Args {
    arguments
        .iter()
        .map(|(name, value)| {
            let shown = if is_secret_name(name) {
                Value::String(REDACTED.into())
            } else {
                redact(value)
            };
            (name.clone(), shown)
        })
        .collect()
}

/// Stable identity for "the same call again": a 16-hex-digit digest of the tool and its
/// redacted arguments in a canonical JSON layout, so the same call always gets the same digest.
pub fn fingerprint(tool: &str, arguments: &Args) -> String {
    let identity: Map<String, Value> = safe_arguments(arguments)
        .into_iter()
        .filter(|(name, _)| !DISPLAY_ONLY_ARGUMENTS.contains(&name.as_str()))
        .collect();
    let payload = python_json(&json!({"tool": tool, "arguments": Value::Object(identity)}));
    Sha256::digest(payload.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..16]
        .to_owned()
}

/// Expand a leading `~` against `HOME`; `None` when `~` is used and `HOME` is unset.
fn expand_home(host: &dyn Host, text: &str) -> Option<PathBuf> {
    match text
        .strip_prefix("~/")
        .or_else(|| (text == "~").then_some(""))
    {
        Some(rest) => host.var("HOME").map(|home| Path::new(&home).join(rest)),
        None => Some(PathBuf::from(text)),
    }
}

/// Where the journal lives, or `None` when logging is switched off or cannot be located.
pub fn log_path(host: &dyn Host) -> Option<PathBuf> {
    match read_env(host, LOG_ENV).filter(|text| !text.is_empty()) {
        Some(override_text) => {
            if ["off", "0", "none", "false"].contains(&override_text.trim().to_lowercase().as_str())
            {
                None
            } else {
                expand_home(host, &override_text)
            }
        }
        None => expand_home(host, DEFAULT_PATH),
    }
}

/// The live journal's size ceiling; a non-positive or malformed value means the default.
pub fn max_bytes(host: &dyn Host) -> u64 {
    read_env(host, MAX_BYTES_ENV)
        .and_then(|text| text.trim().parse::<i64>().ok())
        .filter(|value| *value > 0)
        .map_or(DEFAULT_MAX_BYTES, |value| value as u64)
}

/// Where the previous journal generation is kept after rotation: `<name>.1`.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    path.with_file_name(name)
}

/// Per-tool maximum reuse age in seconds, parsed from a JSON object. Anything malformed or
/// non-positive yields no entry; a malformed whole policy yields an empty map (reuse disabled).
pub fn parse_reuse_policy(raw: &str) -> BTreeMap<String, f64> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(name, value)| {
            let age = match value {
                Value::Number(number) => number.as_f64(),
                Value::String(text) => text.trim().parse::<f64>().ok(),
                Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
                _ => None,
            }?;
            (age > 0.0).then(|| (name.clone(), age))
        })
        .collect()
}

/// The configured reuse policy, read from `SEOHEAD_REUSE_POLICY`. Empty means never reuse.
pub fn reuse_policy(host: &dyn Host) -> BTreeMap<String, f64> {
    read_env(host, REUSE_POLICY_ENV)
        .map(|raw| parse_reuse_policy(&raw))
        .unwrap_or_default()
}

/// Secret-named keys replaced, everything else kept byte-for-byte (no truncation): a reused
/// answer played back truncated would be a quieter version of the staleness bug this guards.
fn redact_secret_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| {
                    let shown = if is_secret_name(key) {
                        Value::String(REDACTED.into())
                    } else {
                        redact_secret_keys(item)
                    };
                    (key.clone(), shown)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_secret_keys).collect()),
        other => other.clone(),
    }
}

/// The result, if it is a JSON object small enough to journal for later reuse.
fn reusable_payload(result: &Value) -> Option<Value> {
    if !result.is_object() {
        return None;
    }
    let redacted = redact_secret_keys(result);
    // A stored answer that redaction altered is a corrupted copy: replaying it would hand
    // back `[redacted]` where the original had data, so an altered result is never reused.
    if redacted != *result {
        return None;
    }
    (python_json(&redacted).chars().count() <= MAX_REUSABLE_RESULT_BYTES).then_some(redacted)
}

/// Parse a `%Y-%m-%dT%H:%M:%SZ` UTC stamp into seconds since the epoch.
pub fn parse_zulu_seconds(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let field = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    let month_days = days_in_month(year, month)?;
    if !(1..=month_days).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

fn days_in_month(year: i64, month: i64) -> Option<i64> {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    })
}

/// Days since 1970-01-01 of a proleptic Gregorian date (inverse of the timeutil conversion).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

mod journal;
pub use journal::*;

/// Lines of a file, newest first, read backwards in 64 KiB blocks so reading recent journal
/// entries never materialises the whole file. An invalid-UTF8 line still yields lossy text,
/// which then fails JSON parsing exactly like a corrupt line does.
struct ReverseLines {
    file: fs::File,
    /// Bytes of the file read so far that have not been returned as lines yet.
    buf: Vec<u8>,
    /// File offset the buffered bytes start at; reads continue below it.
    pos: u64,
}

impl ReverseLines {
    fn open(path: &Path) -> Option<Self> {
        let file = fs::File::open(path).ok()?;
        let pos = file.metadata().ok()?.len();
        Some(Self {
            file,
            buf: Vec::new(),
            pos,
        })
    }
}

impl Iterator for ReverseLines {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        const BLOCK: u64 = 64 * 1024;
        loop {
            if let Some(idx) = self.buf.iter().rposition(|byte| *byte == b'\n') {
                let line = self.buf.split_off(idx + 1);
                self.buf.truncate(idx);
                return Some(String::from_utf8_lossy(&line).into_owned());
            }
            if self.pos == 0 {
                if self.buf.is_empty() {
                    return None;
                }
                let first = std::mem::take(&mut self.buf);
                return Some(String::from_utf8_lossy(&first).into_owned());
            }
            let take = self.pos.min(BLOCK);
            self.pos -= take;
            if self.file.seek(SeekFrom::Start(self.pos)).is_err() {
                return None;
            }
            let mut block = vec![0_u8; take as usize];
            if self.file.read_exact(&mut block).is_err() {
                return None;
            }
            block.extend_from_slice(&self.buf);
            self.buf = block;
        }
    }
}

/// Append `line`, first rotating the live file to its backup when the append would cross the
/// ceiling. Creating the parent directory is part of the append.
fn append_line(path: &Path, line: &str, max_bytes: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(metadata) = fs::metadata(path) {
        if metadata.len() + line.len() as u64 > max_bytes {
            fs::rename(path, backup_path(path))?;
        }
    }
    let mut handle = OpenOptions::new().create(true).append(true).open(path)?;
    handle.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests;

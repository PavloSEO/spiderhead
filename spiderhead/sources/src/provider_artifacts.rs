//! Read bounded, sanitized summaries from explicitly retained provider artifacts.
//!
//! A `local-artifact:<sha256>` reference resolves to `provider-<digest16>.json`
//! inside a private artifact directory. The reader never returns the stored raw
//! envelope: every field is allowlisted, URLs are cut to their public identity and
//! free text is whitespace-collapsed and length-capped.

use spiderhead_core::filesystem::{file_identity, link_identity, NoFollow, StatCompat};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::credentials::is_private_mode;
use crate::pystr;

/// `MAX_ARTIFACT_BYTES` — one retained artifact's UTF-8 budget (16 MiB).
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
/// `MAX_SAMPLES` — the page-size ceiling and the stored-sample ceiling.
pub const MAX_SAMPLES: i64 = 25;
/// `MAX_AUDITS_PER_SAMPLE` — sorted-audit cap per returned sample.
pub const MAX_AUDITS_PER_SAMPLE: usize = 100;

/// Errors this module raises. All are invalid-input errors, except filesystem failures that occur
/// only when the directory itself cannot be read.
#[derive(Debug, Error)]
pub enum PaError {
    /// Invalid input.
    #[error("{0}")]
    Invalid(String),
    /// Value of the wrong type.
    #[error("{0}")]
    TypeErr(String),
    /// Filesystem failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl PaError {
    /// Error class name of this variant.
    pub fn python_class(&self) -> &'static str {
        match self {
            PaError::Invalid(_) => "ValueError",
            PaError::TypeErr(_) => "TypeError",
            PaError::Io(_) => "OSError",
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, PaError> {
    Err(PaError::Invalid(message.into()))
}

/// Split a URL into its hostname and port, as the standard URL parser does.
fn hostinfo(netloc: &str) -> (String, Option<Result<u16, ()>>) {
    let netloc = netloc.rsplit('@').next().unwrap_or(netloc);
    let (host, port) = if let Some(bracketed) = netloc.strip_prefix('[') {
        let (host, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        (host.to_owned(), rest.strip_prefix(':').map(str::to_owned))
    } else {
        let (host, port) = match netloc.split_once(':') {
            Some((h, p)) if !p.is_empty() => (h.to_owned(), Some(p.to_owned())),
            Some((h, _)) => (h.to_owned(), None),
            None => (netloc.to_owned(), None),
        };
        (host, port)
    };
    let port = port.map(|p| {
        if p.bytes().all(|b| b.is_ascii_digit()) {
            p.parse::<u32>()
                .ok()
                .filter(|v| *v <= 65535)
                .map(|v| v as u16)
                .ok_or(())
        } else {
            Err(())
        }
    });
    (host.to_lowercase(), port)
}

/// `_public_url` — retain the page identity while dropping query/fragment data
/// that may contain tokens.
fn public_url(value: &Value) -> Value {
    let Some(text) = value.as_str() else {
        return Value::Null;
    };
    let Ok(parts) = spiderhead_parse::py_url::urlsplit(text) else {
        return Value::Null;
    };
    if parts.scheme != "http" && parts.scheme != "https" {
        return Value::Null;
    }
    let (hostname, port) = hostinfo(&parts.netloc);
    let port = match port {
        Some(Ok(p)) => Some(p),
        Some(Err(())) => return Value::Null,
        None => None,
    };
    if hostname.is_empty() {
        return Value::Null;
    }
    let mut host = hostname;
    if let Some(p) = port {
        host = format!("{host}:{p}");
    }
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    Value::String(format!("{}://{host}{path}", parts.scheme))
}

/// `(https?://[^\s?#]+)[?#][^\s]+` → `\1` applied to one whitespace-free word.
fn strip_url_tail(word: &str) -> &str {
    let after = word
        .strip_prefix("http://")
        .or_else(|| word.strip_prefix("https://"));
    let Some(rest) = after else { return word };
    let keep = rest.find(['?', '#']).map(|i| &rest[..i]).unwrap_or(rest);
    if keep.is_empty() {
        return word;
    }
    let prefix_len = word.len() - rest.len() + keep.len();
    &word[..prefix_len]
}

/// `_small_text` — collapse whitespace, cut URL query/fragment tails, truncate.
fn small_text(value: &Value, limit: usize) -> Value {
    let Some(text) = value.as_str() else {
        return Value::Null;
    };
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let scrubbed = collapsed
        .split(' ')
        .map(strip_url_tail)
        .collect::<Vec<_>>()
        .join(" ");
    Value::String(scrubbed.chars().take(limit).collect())
}

/// True for a JSON integer or float; booleans are rejected.
fn is_number_or_none(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Bool(_) | Value::Number(_))
}

/// SHA-256 of the stored envelope serialized with sorted keys.
fn digest_of(stored: &Value) -> String {
    use sha2::{Digest, Sha256};
    let text = crate::pyjson::dumps_opts(stored, true, true);
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn mode_of(path: &Path) -> std::io::Result<u32> {
    Ok(path.symlink_metadata()?.mode() & 0o7777)
}

/// Owner uid where uid-based checks exist; `None` elsewhere, which disables
/// the ownership comparisons below.
#[cfg(unix)]
fn uid() -> Option<u32> {
    Some(unsafe { libc::getuid() })
}

/// Owner uid where uid-based checks exist.
#[cfg(not(unix))]
fn uid() -> Option<u32> {
    None
}

/// `st_uid` where the platform reports one.
#[cfg(unix)]
fn owner_of(meta: &std::fs::Metadata) -> Option<u32> {
    Some(std::os::unix::fs::MetadataExt::uid(meta))
}

/// `st_uid` where the platform reports one.
#[cfg(not(unix))]
fn owner_of(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

/// `read_pagespeed_artifact(artifact_ref, artifact_dir, *, offset=0, limit=25)` —
/// return a field-allowlisted PSI sample page; never return the saved raw envelope.
pub fn read_pagespeed_artifact(
    artifact_ref: &Value,
    artifact_dir: &Value,
    offset: Option<&Value>,
    limit: Option<&Value>,
) -> Result<Value, PaError> {
    let default_offset = json!(0);
    let default_limit = json!(25);
    let offset = offset.unwrap_or(&default_offset);
    let limit = limit.unwrap_or(&default_limit);
    // A falsy reference is treated as empty; a non-string reference is a type error.
    let reference = match artifact_ref {
        v if !pystr::truthy(Some(v)) => "",
        Value::String(s) => s.as_str(),
        other => {
            return Err(PaError::TypeErr(format!(
                "expected string or bytes-like object, got '{}'",
                pystr::type_name(other)
            )))
        }
    };
    let digest = reference.strip_prefix("local-artifact:").filter(|d| {
        d.len() == 64
            && d.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    let Some(digest) = digest else {
        return invalid("artifact_ref must be a local provider artifact reference");
    };
    if !pystr::is_py_int(offset)
        || offset.as_i64().unwrap_or(-1) < 0
        || !pystr::is_py_int(limit)
        || !(1..=MAX_SAMPLES).contains(&limit.as_i64().unwrap_or(0))
    {
        return invalid("offset must be nonnegative and limit must be 1..25");
    }
    let offset_i = offset.as_i64().unwrap_or(0);
    let limit_i = limit.as_i64().unwrap_or(0);
    let root = match artifact_dir {
        Value::String(s) => PathBuf::from(s),
        other => {
            return Err(PaError::TypeErr(format!(
                "argument should be a str or an os.PathLike object where __fspath__ \
                 returns a str, not '{}'",
                pystr::type_name(other)
            )))
        }
    };
    if root.is_symlink() || !root.is_dir() {
        return invalid("artifact_dir must be an existing regular directory");
    }
    let root = root.canonicalize()?;
    if !is_private_mode(mode_of(&root)?) {
        return invalid("artifact_dir must be private");
    }
    if uid().is_some_and(|u| {
        root.symlink_metadata()
            .ok()
            .and_then(|m| owner_of(&m))
            .unwrap_or(u + 1)
            != u
    }) {
        return invalid("artifact_dir must be owned by the current user");
    }
    let path = root.join(format!("provider-{}.json", &digest[..16]));
    let Ok(info) = path.symlink_metadata() else {
        return invalid("provider artifact was not found");
    };
    if info.file_type().is_symlink()
        || !info.is_file()
        || link_identity(&path).map(|i| i.2 != 1).unwrap_or(true)
        || info.len() > MAX_ARTIFACT_BYTES
        || !is_private_mode(info.mode())
        || uid().is_some_and(|u| owner_of(&info) != Some(u))
    {
        return invalid("provider artifact is unsafe or exceeds its byte limit");
    }
    let resolved = path.canonicalize()?;
    if resolved.parent() != Some(root.as_path()) {
        return invalid("provider artifact escapes artifact_dir");
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .no_follow()
        .open(&resolved);
    let file = match file {
        Ok(file) => file,
        Err(_) => return invalid("provider artifact is unsafe or exceeds its byte limit"),
    };
    let opened = match file.metadata() {
        Ok(m) => m,
        Err(_) => return invalid("provider artifact changed while it was being opened"),
    };
    let opened_id = match file_identity(&file) {
        Ok(id) => id,
        Err(_) => return invalid("provider artifact changed while it was being opened"),
    };
    let info_id = link_identity(&path).unwrap_or((0, 0, 0));
    if !opened.is_file()
        || opened_id.2 != 1
        || opened.len() > MAX_ARTIFACT_BYTES
        || (opened_id.0, opened_id.1) != (info_id.0, info_id.1)
        || uid().is_some_and(|u| owner_of(&opened) != Some(u))
    {
        return invalid("provider artifact changed while it was being opened");
    }
    use std::io::Read;
    let mut raw = Vec::new();
    let _ = file.take(MAX_ARTIFACT_BYTES + 1).read_to_end(&mut raw);
    if raw.len() as u64 > MAX_ARTIFACT_BYTES {
        return invalid("provider artifact exceeds its byte limit");
    }
    let Ok(stored) = serde_json::from_slice::<Value>(&raw) else {
        return invalid("provider artifact is not valid JSON");
    };
    let evidence = stored.get("evidence");
    let result = stored.get("result");
    let shape_ok = stored.is_object()
        && stored.as_object().map(|m| m.len()) == Some(2)
        && evidence.is_some_and(Value::is_object)
        && spiderhead_core::formats::matches(
            evidence
                .and_then(|e| e.get("format"))
                .and_then(Value::as_str),
            "spiderhead.provider-evidence.v1",
        )
        && evidence.and_then(|e| e.get("provider")) == Some(&json!("pagespeed"))
        && matches!(
            evidence
                .and_then(|e| e.get("operation"))
                .and_then(Value::as_str),
            Some("mobile_samples" | "desktop_samples")
        )
        && result.is_some_and(Value::is_object);
    if !shape_ok {
        return invalid("artifact is not a retained PageSpeed collection");
    }
    if digest_of(&stored) != digest {
        return invalid("provider artifact reference does not match its content");
    }
    let result = result.cloned().unwrap_or_default();
    let samples = result.get("samples").cloned().unwrap_or(Value::Null);
    let valid = samples.as_array().is_some_and(|list| {
        list.len() <= MAX_SAMPLES as usize && list.iter().all(Value::is_object)
    });
    if !valid {
        return invalid("PageSpeed artifact has an invalid sample list");
    }
    let list = samples.as_array().cloned().unwrap_or_default();
    let mut selected = Vec::new();
    for sample in list
        .iter()
        .skip(offset_i.max(0) as usize)
        .take(limit_i.max(0) as usize)
    {
        let categories = sample.get("categories").cloned().unwrap_or(Value::Null);
        let mut safe_categories = Map::new();
        if let Value::Object(map) = &categories {
            for (name, category) in map.iter().take(16) {
                if !category.is_object() {
                    continue;
                }
                let score = category.get("score").cloned().unwrap_or(Value::Null);
                let score = if score.is_null() || score.is_number() {
                    score
                } else {
                    Value::Null
                };
                safe_categories.insert(
                    name.chars().take(64).collect::<String>(),
                    json!({
                        "score": score,
                        "title": small_text(category.get("title").unwrap_or(&Value::Null), 100),
                    }),
                );
            }
        }
        let audits = sample.get("audits").cloned().unwrap_or(Value::Null);
        let mut safe_audits = Map::new();
        if let Value::Object(map) = &audits {
            let mut sorted: Vec<(&String, &Value)> = map.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            for (name, audit) in sorted.into_iter().take(MAX_AUDITS_PER_SAMPLE) {
                if !audit.is_object() {
                    continue;
                }
                let score = audit.get("score").cloned().unwrap_or(Value::Null);
                let numeric = audit.get("numeric_value").cloned().unwrap_or(Value::Null);
                safe_audits.insert(
                    name.chars().take(100).collect::<String>(),
                    json!({
                        "score": if is_number_or_none(&score) { score } else { Value::Null },
                        "numeric_value": if is_number_or_none(&numeric) { numeric } else { Value::Null },
                        "display_value": small_text(audit.get("display_value").unwrap_or(&Value::Null), 200),
                        "score_display_mode": small_text(audit.get("score_display_mode").unwrap_or(&Value::Null), 40),
                    }),
                );
            }
        }
        let strategy = sample.get("strategy").cloned().unwrap_or(Value::Null);
        let state = sample.get("state").cloned().unwrap_or(Value::Null);
        selected.push(json!({
            "url": public_url(sample.get("url").unwrap_or(&Value::Null)),
            "strategy": if matches!(strategy.as_str(), Some("mobile" | "desktop")) { strategy } else { Value::Null },
            "state": if matches!(state.as_str(), Some("complete" | "failed")) { state } else { json!("unavailable") },
            "final_url": public_url(sample.get("final_url").unwrap_or(&Value::Null)),
            "fetch_time": small_text(sample.get("fetch_time").unwrap_or(&Value::Null), 40),
            "retrieved_at": small_text(sample.get("fetch_time").unwrap_or(&Value::Null), 40),
            "lighthouse_version": small_text(sample.get("lighthouse_version").unwrap_or(&Value::Null), 40),
            "categories": Value::Object(safe_categories),
            "audits": Value::Object(safe_audits),
            "lab_only": true,
        }));
    }
    Ok(json!({
        "schema_version": "pagespeed-artifact-summary.v1",
        "provider": "pagespeed",
        "operation": evidence.and_then(|e| e.get("operation")).cloned().unwrap_or(Value::Null),
        "retrieved_at": small_text(evidence.and_then(|e| e.get("retrieved_at")).unwrap_or(&Value::Null), 40),
        "total": list.len(),
        "offset": offset_i,
        "limit": limit_i,
        "has_more": (offset_i + selected.len() as i64) < list.len() as i64,
        "samples": selected,
        "lab_only": true,
    }))
}

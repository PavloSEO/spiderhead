//! Resolve a saved audit without replacing it with a neighboring export.

use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::audit_v2::{audit_v2_path, AuditV2Reader};
use crate::error::StoreError;
use crate::scan_artifact::{loads, MAX_JSON_BYTES};

/// Magic bytes at the start of every SQLite database file.
pub const SQLITE_MAGIC: &[u8] = b"SQLite format 3\x00";
/// Suffix of an audit.v2 companion file (see `audit_v2::audit_v2_path`).
pub const AUDIT_V2_SIDECAR_SUFFIX: &str = ".audit-v2.sqlite";

/// One input diagnostic: `code`, `path` and `message`, plus `input` when labelled.
pub type Diagnostic = Map<String, Value>;

fn diagnostic(code: &str, path: &str, message: &str) -> Diagnostic {
    let mut d = Map::new();
    d.insert("code".into(), json!(code));
    d.insert("path".into(), json!(path));
    d.insert("message".into(), json!(message));
    d
}

/// Path named by a JSON string value, or `None` for any other value.
fn path_of(value: &Value) -> Option<PathBuf> {
    value.as_str().map(PathBuf::from)
}

/// True when the value names an existing file that begins with the SQLite magic bytes.
pub fn is_sqlite_input(value: &Value) -> bool {
    let Some(path) = path_of(value) else {
        return false;
    };
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 16];
    matches!(file.read_exact(&mut head), Ok(())) && head == *SQLITE_MAGIC
}

/// Whether two paths name the same file (device and inode), like `os.path.samefile`.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    use spiderhead_core::filesystem::path_identity;
    match (path_identity(a), path_identity(b)) {
        (Ok(x), Ok(y)) => (x.0, x.1) == (y.0, y.1),
        _ => false,
    }
}

/// Compare an adjacent `audit.json` with the scan's internal audit digest.
pub fn adjacent_diagnostics(path: &Path, digest: &str) -> Vec<Diagnostic> {
    let adjacent = path.with_file_name("audit.json");
    if std::fs::symlink_metadata(&adjacent).is_err() {
        return Vec::new();
    }
    if same_file(path, &adjacent) {
        return Vec::new();
    }
    let read = || -> std::io::Result<Vec<u8>> {
        let mut content = Vec::new();
        std::fs::File::open(&adjacent)?
            .take(MAX_JSON_BYTES as u64 + 1)
            .read_to_end(&mut content)?;
        Ok(content)
    };
    match read() {
        Ok(content) if content.len() as i64 > MAX_JSON_BYTES => vec![diagnostic(
            "adjacent_audit_unavailable",
            "audit.json",
            "Could not validate adjacent audit.json; used the scan's internal audit.",
        )],
        Ok(content) => {
            let hex = format!("{:x}", Sha256::digest(&content));
            if hex == digest {
                Vec::new()
            } else {
                vec![diagnostic(
                    "adjacent_audit_mismatch",
                    "audit.json",
                    "Adjacent audit.json differs; used the scan's internal audit.",
                )]
            }
        }
        Err(_) => vec![diagnostic(
            "adjacent_audit_unavailable",
            "audit.json",
            "Could not validate adjacent audit.json; used the scan's internal audit.",
        )],
    }
}

/// A resolved audit: a complete document, or a validated streaming audit.v2 reader.
pub enum AuditSource {
    /// The complete document (an `audit.json`, a scan's internal audit, or a given object).
    Document(Value),
    /// The audit.v2 companion of a scan, whose collections stream.
    Streaming(Box<AuditV2Reader>),
}

impl AuditSource {
    /// The document, or the reader's header (collections replaced by markers).
    pub fn header(&self) -> &Value {
        match self {
            Self::Document(document) => document,
            Self::Streaming(reader) => &reader.header,
        }
    }

    /// The reader, when the audit streams.
    pub fn reader(&self) -> Option<&AuditV2Reader> {
        match self {
            Self::Streaming(reader) => Some(reader),
            Self::Document(_) => None,
        }
    }

    /// Materialize the complete legacy document (streaming sources stay under `MAX_JSON_BYTES`).
    pub fn into_document(self) -> Result<Value, StoreError> {
        match self {
            Self::Document(document) => Ok(document),
            Self::Streaming(reader) => reader.materialize_legacy(MAX_JSON_BYTES),
        }
    }
}

/// Return the complete legacy document and separate diagnostics, without network access.
///
/// The explicit compatibility path stays bounded at ``MAX_JSON_BYTES``. Callers that can
/// stream audit.v2 collections should use [`resolve_audit_source`] instead.
pub fn resolve_audit_input(value: &Value) -> Result<(Value, Vec<Diagnostic>), StoreError> {
    let (source, diagnostics) = resolve_audit_source(value)?;
    Ok((source.into_document()?, diagnostics))
}

/// Return a document or validated audit.v2 reader, plus input diagnostics.
///
/// An audit.v2 companion is resolved to its native scan (with a notice). A scan that has a
/// companion yields the validated streaming reader; otherwise its stored audit document.
pub fn resolve_audit_source(value: &Value) -> Result<(AuditSource, Vec<Diagnostic>), StoreError> {
    if value.is_object() {
        return Ok((AuditSource::Document(value.clone()), Vec::new()));
    }
    let Some(path) = path_of(value) else {
        return Err(StoreError::Invalid(
            "audit input must be a path or a document".into(),
        ));
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(stripped) = name.strip_suffix(AUDIT_V2_SIDECAR_SUFFIX) {
        // A companion is not a scan: read the native scan it belongs to, and say so.
        let scan = path.with_file_name(stripped);
        if !is_sqlite_input(&json!(scan.display().to_string())) {
            return Err(StoreError::Invalid(format!(
                "{name} is an audit.v2 companion without its native scan"
            )));
        }
        let (source, notices) = resolve_audit_source(&json!(scan.display().to_string()))?;
        let notice = diagnostic(
            "audit_v2_sidecar",
            &name,
            "Used the native scan this audit.v2 companion belongs to.",
        );
        let mut all = vec![notice];
        all.extend(notices);
        return Ok((source, all));
    }
    if !is_sqlite_input(value) {
        // The compatibility path stays bounded at MAX_JSON_BYTES like adjacent_diagnostics.
        let mut text = String::new();
        std::fs::File::open(&path)?
            .take(MAX_JSON_BYTES as u64 + 1)
            .read_to_string(&mut text)?;
        if text.len() as i64 > MAX_JSON_BYTES {
            return Err(StoreError::Invalid(format!(
                "audit JSON exceeds the {MAX_JSON_BYTES} byte limit"
            )));
        }
        return Ok((AuditSource::Document(loads(&text, "audit")?), Vec::new()));
    }
    if audit_v2_path(&path).exists() {
        let reader = AuditV2Reader::new(&path, true, false, None)?;
        return Ok((AuditSource::Streaming(Box::new(reader)), Vec::new()));
    }
    let con =
        spiderhead_core::sqlite::open_readonly(&path, std::time::Duration::from_secs(5), &[])?;
    let row: Option<(String, String)> = con
        .query_row(
            "SELECT document_json, sha256 FROM audit WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((document_json, digest)) = row else {
        return Err(StoreError::Invalid(
            "saved scan has no current audit".into(),
        ));
    };
    Ok((
        AuditSource::Document(loads(&document_json, "audit")?),
        adjacent_diagnostics(&path, &digest),
    ))
}

/// Accept an already-parsed audit document, or a path to one, without changing its
/// contents. ``label`` names the argument in the error, so a caller taking more than one
/// audit (a before/after compare, a source/target segment diff) can say which one is bad.
pub fn load_audit_document(
    value: &Value,
    label: &str,
    diagnostics: Option<&mut Vec<Diagnostic>>,
) -> Result<Value, StoreError> {
    if value.is_object() {
        return Ok(value.clone());
    }
    if path_of(value).is_some() {
        let (document, notices) = resolve_audit_input(value)?;
        push_notices(diagnostics, notices, label);
        return Ok(document);
    }
    Err(StoreError::Invalid(format!(
        "{label} required: an audit document or a path to its JSON file"
    )))
}

/// Accept a JSON object, JSON path, scan path, or streaming audit.v2 source.
pub fn load_audit_source(
    value: &Value,
    label: &str,
    diagnostics: Option<&mut Vec<Diagnostic>>,
) -> Result<AuditSource, StoreError> {
    if value.is_object() {
        return Ok(AuditSource::Document(value.clone()));
    }
    if path_of(value).is_some() {
        let (source, notices) = resolve_audit_source(value)?;
        push_notices(diagnostics, notices, label);
        return Ok(source);
    }
    Err(StoreError::Invalid(format!(
        "{label} required: an audit document or a path to its JSON file"
    )))
}

fn push_notices(sink: Option<&mut Vec<Diagnostic>>, notices: Vec<Diagnostic>, label: &str) {
    if let Some(sink) = sink {
        for mut notice in notices {
            notice.insert("input".into(), json!(label));
            sink.push(notice);
        }
    }
}

/// A report destination cannot replace its source scan, including an alias.
pub fn protects_scan_input(value: &Value, targets: &[PathBuf]) -> bool {
    let Some(source) = path_of(value) else {
        return false;
    };
    is_sqlite_input(value)
        && targets
            .iter()
            .any(|target| target.exists() && same_file(&source, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spiderhead-inputs-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // regression: the explicit audit path must refuse inputs over MAX_JSON_BYTES
    // instead of reading the whole file into memory
    #[test]
    fn audit_input_over_the_json_cap_is_refused() {
        let dir = scratch("cap");
        let path = dir.join("audit.json");
        std::fs::write(&path, vec![b' '; MAX_JSON_BYTES as usize + 1]).unwrap();
        let outcome = resolve_audit_source(&json!(path.display().to_string()));
        match outcome {
            Err(StoreError::Invalid(m)) => assert!(m.contains("byte limit"), "{m}"),
            other => panic!("expected a byte-limit refusal, got {}", other.is_ok()),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_input_document_still_parses() {
        let dir = scratch("doc");
        let path = dir.join("audit.json");
        std::fs::write(&path, "{\"issues\": []}").unwrap();
        let (source, notices) = resolve_audit_source(&json!(path.display().to_string())).unwrap();
        assert!(matches!(source, AuditSource::Document(_)));
        assert!(notices.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

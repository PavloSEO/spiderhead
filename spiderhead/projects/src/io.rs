//! Shared open/publish primitives for bounded project JSON documents
//!
//! Document owners keep their own validation; these helpers keep the open-and-check dance
//! in one place while [`crate::runtime::write_document`] owns the lock, the retained
//! previous revision and the byte budget.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::ProjectsResult;
use crate::runtime::{read_document, write_document};
use crate::workspace::load as workspace_load;

/// `load_document`: open the workspace and read one document, or its empty default.
///
/// Returns `(root, project, document)`; a missing document falls back to
/// `default(project_uuid)`.
pub fn load_document(
    directory: &Path,
    name: &str,
    default: &dyn Fn(&str) -> Value,
) -> ProjectsResult<(PathBuf, Value, Value)> {
    let (root, project) = workspace_load(directory)?;
    let document = match read_document(&root, name)? {
        Some(document) => document,
        None => default(project["project_uuid"].as_str().unwrap_or_default()),
    };
    Ok((root, project, document))
}

/// `check_owner`: reject a document naming another project or an unsupported format.
pub fn check_owner(
    document: &Value,
    project_uuid: &str,
    fmt: &str,
    label: &str,
) -> ProjectsResult<()> {
    if document.get("format").and_then(Value::as_str) != Some(fmt)
        || document.get("project_uuid").and_then(Value::as_str) != Some(project_uuid)
    {
        return Err(crate::error::value(format!(
            "{label} belongs to another project or format"
        )));
    }
    Ok(())
}

/// `save_revision`: publish one document with optimistic concurrency on its revision.
pub fn save_revision(root: &Path, name: &str, document: &mut Value) -> ProjectsResult<()> {
    let expected = document["revision"].as_i64().unwrap_or(0);
    document["revision"] = serde_json::json!(expected + 1);
    write_document(root, name, document, Some(expected))
}

/// `save_checked`: publish after confirming the caller's revision expectation.
pub fn save_checked(
    root: &Path,
    name: &str,
    document: &mut Value,
    expected_revision: i64,
    label: &str,
) -> ProjectsResult<()> {
    if document["revision"].as_i64() != Some(expected_revision) {
        return Err(crate::error::value(format!("{label} revision conflict")));
    }
    document["revision"] = serde_json::json!(expected_revision + 1);
    write_document(root, name, document, None)
}

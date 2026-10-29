//! Project manifest format constants.

use crate::artifacts::PROJECT_V1;

/// Format identifier written into a project manifest: `spiderhead.project.v1`.
pub const PROJECT_FORMAT: &str = PROJECT_V1;

/// Manifest schema version written next to [`PROJECT_FORMAT`].
pub const PROJECT_VERSION: u32 = 1;

/// Layout version of `project.json` written by this build. Manifests without the
/// `schema_version` key are version 1.
pub const SCHEMA_VERSION: u64 = 1;

/// Bring a loaded manifest up to [`SCHEMA_VERSION`]. Version 1 needs no change; later
/// versions add their step here, one `from -> from + 1` arm at a time.
///
/// Errors when the manifest was written by a newer build.
pub fn migrate(
    mut document: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let found = match document.get("schema_version") {
        None => 1,
        Some(value) => value
            .as_u64()
            .filter(|v| *v >= 1)
            .ok_or("project.json schema_version must be a positive integer")?,
    };
    if found > SCHEMA_VERSION {
        return Err(format!(
            "project.json schema_version {found} is newer than this build supports ({SCHEMA_VERSION})"
        ));
    }
    let mut at = found;
    while at < SCHEMA_VERSION {
        at += 1;
    }
    document.insert("schema_version".into(), at.into());
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_manifest_without_schema_version_loads_as_version_one() {
        let doc = json!({"format": PROJECT_FORMAT})
            .as_object()
            .cloned()
            .unwrap();
        assert_eq!(migrate(doc).unwrap()["schema_version"], json!(1));
    }

    #[test]
    fn a_newer_manifest_is_refused() {
        let doc = json!({"schema_version": 99}).as_object().cloned().unwrap();
        assert!(migrate(doc).unwrap_err().contains("newer"));
    }

    #[test]
    fn project_format_and_version_match_the_reference_workspace() {
        assert_eq!(PROJECT_FORMAT, "spiderhead.project.v1");
        assert_eq!(PROJECT_VERSION, 1);
    }
}

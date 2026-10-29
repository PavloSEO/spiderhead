//! Machine-readable core identity for `spiderhead version --json`.
//!
//! Reads only constants and the build manifest the caller passes in. The file read stays at the edge, so the output is pure and testable.
//! No network, no paths, no config values: the output is safe to show in a UI or paste into a bug report.

use serde_json::{json, Value};
use thiserror::Error;

use crate::artifacts::SCAN_FORMATS;
use crate::ledger_versions::{FORMAT_VERSION as LEDGER_FORMAT, USER_VERSION};
use crate::project_format::{PROJECT_FORMAT, PROJECT_VERSION};

/// Identifier of this document, also reported as its contract version.
pub const CORE_INFO_FORMAT: &str = "spiderhead.core-info.v1";

/// The build manifest shipped with the package, when it was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Package version recorded in the manifest.
    pub version: String,
    /// Full lowercase Git revision of the build.
    pub revision: String,
    /// Build time (`...Z`), absent in older manifests.
    pub built_at: Option<String>,
}

/// Inputs of [`core_info`].
#[derive(Debug, Clone, Copy)]
pub struct CoreInfoInput<'a> {
    /// Package version reported as `package_version`.
    pub package_version: &'a str,
    /// Verified build manifest, or `None` for a dev tree without one.
    pub provenance: Option<&'a Provenance>,
    /// CLI command names reported under `supported_tools.cli`.
    pub commands: &'a [&'a str],
    /// MCP tool names reported under `supported_tools.mcp`.
    pub mcp_tools: &'a [&'a str],
    /// The revision a desktop build pins; must be a full lowercase Git SHA when given.
    pub required_core_revision: Option<&'a str>,
}

/// Failure of [`core_info`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreInfoError {
    /// The pinned revision is not a 40-character lowercase hexadecimal Git SHA.
    #[error("required_core_revision must be a full lowercase Git SHA")]
    InvalidRequiredRevision,
}

/// The revision of a verified build, or `None` without a manifest.
pub fn revision_of(provenance: Option<&Provenance>) -> Option<&str> {
    provenance.map(|found| found.revision.as_str())
}

/// Build metadata block: `verified` with the manifest, `unavailable` without it.
pub fn build_metadata(provenance: Option<&Provenance>) -> Value {
    match provenance {
        None => json!({
            "state": "unavailable",
            "revision": null,
            "built_at": null,
            "reason": "verified package build provenance is unavailable",
        }),
        Some(found) => json!({
            "state": "verified",
            "revision": found.revision,
            "built_at": found.built_at,
            "reason": match found.built_at {
                Some(_) => Value::Null,
                None => json!("build time is unavailable in this older package manifest"),
            },
        }),
    }
}

fn is_full_git_sha(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The `spiderhead.core-info.v1` document for this build.
pub fn core_info(input: &CoreInfoInput<'_>) -> Result<Value, CoreInfoError> {
    if let Some(required) = input.required_core_revision {
        if !is_full_git_sha(required) {
            return Err(CoreInfoError::InvalidRequiredRevision);
        }
    }
    let build = build_metadata(input.provenance);
    let built_revision = revision_of(input.provenance);
    let compatible: Value = match (input.required_core_revision, built_revision) {
        (Some(required), Some(built)) => json!(required == built),
        _ => Value::Null,
    };
    let reason = if compatible.is_null() {
        json!("Desktop must provide its pinned core revision and the installed build must be verified")
    } else {
        Value::Null
    };
    Ok(json!({
        "format": CORE_INFO_FORMAT,
        "contract_version": CORE_INFO_FORMAT,
        "package_version": input.package_version,
        "revision": built_revision,
        "build": build,
        "supported_tools": {"cli": input.commands, "mcp": input.mcp_tools},
        "desktop_compatibility": {
            "policy": "exact_core_revision",
            "required_core_revision": input.required_core_revision,
            "compatible": compatible,
            "reason": reason,
        },
        "project": {"format": PROJECT_FORMAT, "version": PROJECT_VERSION},
        "ledger": {"format_version": LEDGER_FORMAT, "user_version": USER_VERSION},
        "scan_formats": SCAN_FORMATS,
        "commands": input.commands,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn manifest() -> Provenance {
        Provenance {
            version: "3.0.0".into(),
            revision: REVISION.into(),
            built_at: Some("2026-10-12T12:00:00Z".into()),
        }
    }

    fn input<'a>(
        provenance: Option<&'a Provenance>,
        required: Option<&'a str>,
        mcp: &'a [&'a str],
    ) -> CoreInfoInput<'a> {
        CoreInfoInput {
            package_version: "3.0.0",
            provenance,
            commands: &["parse", "version"],
            mcp_tools: mcp,
            required_core_revision: required,
        }
    }

    #[test]
    fn document_has_exactly_the_documented_keys_and_types() {
        let info = core_info(&input(None, None, &[])).unwrap();
        let keys: std::collections::BTreeSet<&str> = info
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "format",
            "contract_version",
            "package_version",
            "revision",
            "build",
            "supported_tools",
            "desktop_compatibility",
            "project",
            "ledger",
            "scan_formats",
            "commands",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected);
        assert_eq!(info["format"], CORE_INFO_FORMAT);
        assert_eq!(info["contract_version"], CORE_INFO_FORMAT);
        assert_eq!(info["revision"], Value::Null);
        assert_eq!(info["scan_formats"], json!(["scan.v1", "scan.v2"]));
        assert_eq!(info["commands"], json!(["parse", "version"]));
        assert_eq!(info["supported_tools"]["cli"], json!(["parse", "version"]));
        assert_eq!(info["build"]["state"], "unavailable");
        assert_eq!(
            info["desktop_compatibility"]["policy"],
            "exact_core_revision"
        );
        assert_eq!(info["desktop_compatibility"]["compatible"], Value::Null);
    }

    #[test]
    fn output_carries_no_absolute_paths() {
        let text = core_info(&input(Some(&manifest()), None, &[]))
            .unwrap()
            .to_string();
        assert!(!text.contains("/Users/") && !text.contains("/home/") && !text.contains("C:\\"));
    }

    #[test]
    fn revision_is_none_without_manifest() {
        assert_eq!(revision_of(None), None);
        assert_eq!(revision_of(Some(&manifest())), Some(REVISION));
    }

    #[test]
    fn build_time_and_mcp_tools_are_exposed() {
        let mcp = ["seo_doctor"];
        let info = core_info(&input(Some(&manifest()), None, &mcp)).unwrap();
        assert_eq!(
            info["build"],
            json!({
                "state": "verified",
                "revision": REVISION,
                "built_at": "2026-10-12T12:00:00Z",
                "reason": null,
            })
        );
        assert_eq!(info["supported_tools"]["mcp"], json!(["seo_doctor"]));
    }

    #[test]
    fn older_manifest_without_build_time_gives_a_reason() {
        let old = Provenance {
            built_at: None,
            ..manifest()
        };
        let info = core_info(&input(Some(&old), None, &[])).unwrap();
        assert_eq!(info["build"]["state"], "verified");
        assert_eq!(
            info["build"]["reason"],
            "build time is unavailable in this older package manifest"
        );
    }

    #[test]
    fn desktop_compatibility_uses_the_pinned_revision() {
        let built = manifest();
        let compatible = core_info(&input(Some(&built), Some(REVISION), &[])).unwrap();
        let incompatible = core_info(&input(Some(&built), Some(OTHER), &[])).unwrap();
        assert_eq!(compatible["desktop_compatibility"]["compatible"], true);
        assert_eq!(compatible["desktop_compatibility"]["reason"], Value::Null);
        assert_eq!(incompatible["desktop_compatibility"]["compatible"], false);
        assert_eq!(
            core_info(&input(Some(&built), Some("short"), &[])).unwrap_err(),
            CoreInfoError::InvalidRequiredRevision
        );
        assert_eq!(
            core_info(&input(Some(&built), Some(&REVISION.to_uppercase()), &[])).unwrap_err(),
            CoreInfoError::InvalidRequiredRevision
        );
    }

    #[test]
    fn unverified_build_reports_a_reason_for_compatibility() {
        let info = core_info(&input(None, Some(REVISION), &[])).unwrap();
        assert_eq!(info["desktop_compatibility"]["compatible"], Value::Null);
        assert!(info["desktop_compatibility"]["reason"].is_string());
    }
}

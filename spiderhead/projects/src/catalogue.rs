//! Portable catalogue used by project coverage.
//!
//! The installed crate reads only its bundled generated JSON resource.
//! The release-time generator lives outside this crate.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{ProjectsError, ProjectsResult};

/// Catalogue document format.
pub const CATALOGUE_FORMAT: &str = "spiderhead.project_catalogue.v1";
/// Catalogue document version.
pub const CATALOGUE_VERSION: i64 = 1;

const PACKAGED: &str = include_str!("../data/project_catalogue.json");

fn hash_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9a-f]{64}\z").unwrap());
    &RE
}

/// `_definition_hash`: sha256 of the canonical JSON spelling of a definition.
pub fn definition_hash(definition: &Value) -> String {
    let payload = crate::pyjson::dumps_compact(definition);
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

fn validated_entries(document: &Value) -> ProjectsResult<Map<String, Value>> {
    let object = document.as_object().filter(|o| {
        o.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["format", "version", "entries"].into_iter().collect()
    });
    if object.is_none() {
        return Err(crate::error::value(
            "packaged project catalogue has an unsupported shape",
        ));
    }
    if document["format"].as_str() != Some(CATALOGUE_FORMAT)
        || document["version"].as_i64() != Some(CATALOGUE_VERSION)
    {
        return Err(crate::error::value(
            "packaged project catalogue has an unsupported format or version",
        ));
    }
    let entries = document["entries"]
        .as_array()
        .ok_or_else(|| crate::error::value("packaged project catalogue entries must be a list"))?;
    let mut result = Map::new();
    for entry in entries {
        let object = entry.as_object().ok_or_else(|| {
            crate::error::value("packaged project catalogue has an invalid entry")
        })?;
        let kind = entry
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut required: BTreeSet<&str> = ["id", "kind", "title", "definition_hash", "source"]
            .into_iter()
            .collect();
        if kind == "skill" || kind == "scenario" {
            required.insert("content");
        }
        let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
        if !["check", "skill", "scenario"].contains(&kind) || keys != required {
            return Err(crate::error::value(
                "packaged project catalogue has an invalid entry shape",
            ));
        }
        if required
            .iter()
            .any(|key| object[*key].as_str().unwrap_or_default().is_empty())
        {
            return Err(crate::error::value(
                "packaged project catalogue has invalid entry values",
            ));
        }
        let identifier = entry["id"].as_str().unwrap_or_default();
        let prefix = match kind {
            "check" => "check:",
            "skill" => "skill:",
            "scenario" => "scenario:",
            _ => unreachable!(),
        };
        if !identifier.starts_with(prefix)
            || !hash_re().is_match(entry["definition_hash"].as_str().unwrap_or_default())
        {
            return Err(crate::error::value(
                "packaged project catalogue has an invalid entry identifier or hash",
            ));
        }
        if result.contains_key(identifier) {
            return Err(crate::error::value(format!(
                "packaged project catalogue has duplicate ID {identifier:?}"
            )));
        }
        result.insert(identifier.to_owned(), entry.clone());
    }
    Ok(result)
}

/// `load_catalogue`: load the generated catalogue bundled with the crate.
pub fn load_catalogue() -> ProjectsResult<Map<String, Value>> {
    let document: Value = serde_json::from_str(PACKAGED)
        .map_err(|_| crate::error::value("packaged project catalogue is missing or invalid"))?;
    validated_entries(&document)
}

/// `load_catalogue` shared across callers (the packaged resource never changes).
pub fn catalogue() -> ProjectsResult<&'static Map<String, Value>> {
    static CACHE: LazyLock<Result<Map<String, Value>, String>> =
        LazyLock::new(|| load_catalogue().map_err(|e| e.to_string()));
    match &*CACHE {
        Ok(map) => Ok(map),
        Err(message) => Err(ProjectsError::Value(message.clone())),
    }
}

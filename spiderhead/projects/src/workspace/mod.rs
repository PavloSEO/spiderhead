//! Create and read bounded, portable project workspaces.
//!
//! A workspace is a directory with `project.json`, `log.md` and `scans/`+`reports/` children.
//! Creation is exclusive (`O_EXCL`), the manifest is published last via hard-link, and opening
//! validates the whole document without executing template references.

use spiderhead_core::filesystem::PrivateMode;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;
use uuid::Uuid;

use crate::error::{io, ProjectsError, ProjectsResult};
use crate::urls::{urlsplit, urlunsplit};
use crate::{locks, pytime};
use spiderhead_core::artifacts::PROJECT_V1;
use spiderhead_core::formats::matches;
use spiderhead_core::net::{normalize_domain, normalize_url};

mod compact_scan_keys;
pub use compact_scan_keys::*;

/// Manifest format tag (`spiderhead.project.v1`).
pub const PROJECT_FORMAT: &str = PROJECT_V1;
/// Manifest schema version.
pub const PROJECT_VERSION: i64 = 1;
const MAX_JSON_BYTES: u64 = 1_048_576;

fn reference_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[a-z][a-z0-9._/-]{0,127}\z").unwrap());
    &RE
}

fn fact_name_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z][a-z0-9_]{0,63}").unwrap());
    &RE
}

/// An existing non-symlink directory, resolved.
pub fn directory(path: &Path, label: &str) -> ProjectsResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(crate::error::value(format!("{label} is required")));
    }
    if path.is_symlink() || !path.is_dir() {
        return Err(crate::error::value(format!(
            "{label} must be an existing non-symlink directory"
        )));
    }
    io(path.canonicalize())
}

/// Validate a bounded absolute HTTP(S) site identity and normalize it.
pub fn target(value: &str) -> ProjectsResult<String> {
    if value.len() > 2048 {
        return Err(crate::error::value(
            "target must be a bounded absolute HTTP(S) URL",
        ));
    }
    let raw = urlsplit(value)?;
    if raw.username().is_some()
        || raw.password().is_some()
        || !raw.query.is_empty()
        || !raw.fragment.is_empty()
    {
        return Err(crate::error::value(
            "target site identity must not include credentials, a query, or a fragment",
        ));
    }
    let normalized = normalize_url(value);
    if normalized.is_empty() {
        return Err(crate::error::value(
            "target must be a public HTTP(S) site identity without credentials",
        ));
    }
    let parts = urlsplit(&normalized)?;
    if !parts.query.is_empty() || !parts.fragment.is_empty() || parts.hostname().is_none() {
        return Err(crate::error::value(
            "target site identity must not include a query or fragment",
        ));
    }
    let hostname = parts.hostname().unwrap_or_default();
    let host = normalize_domain(&hostname);
    if host.is_empty() {
        return Err(crate::error::value(
            "target must have a valid public hostname",
        ));
    }
    let port = match parts.port() {
        Err(_) => return Err(crate::error::value("target must have a valid HTTP(S) port")),
        Ok(Some(p)) => format!(":{p}"),
        Ok(None) => String::new(),
    };
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    Ok(urlunsplit(
        &parts.scheme,
        &format!("{host}{port}"),
        path,
        "",
        "",
    ))
}

fn site(target: &str, label: Option<&str>) -> ProjectsResult<Value> {
    let host = normalize_domain(&urlsplit(target)?.hostname().unwrap_or_default());
    if host.is_empty() {
        return Err(crate::error::value(
            "target must have a valid public hostname",
        ));
    }
    if let Some(l) = label {
        if l.trim().is_empty() || l.chars().count() > 128 {
            return Err(crate::error::value(
                "label is a bounded human project label, not a host override",
            ));
        }
    }
    Ok(json!({"host": host, "target": target, "label": label}))
}

/// A bounded list of relative identifiers, deduplicated in order.
pub fn references(value: Option<&Value>, label: &str) -> ProjectsResult<Vec<String>> {
    let Some(list) = value else {
        return Ok(Vec::new());
    };
    let items = list.as_array().ok_or_else(|| {
        crate::error::value(format!("{label} must be a list of at most 100 references"))
    })?;
    if items.len() > 100 {
        return Err(crate::error::value(format!(
            "{label} must be a list of at most 100 references"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let s = item.as_str();
        let ok = s.is_some_and(|s| reference_re().is_match(s) && !s.contains(".."));
        if !ok {
            return Err(crate::error::value(format!(
                "{label} entries must be bounded relative identifiers"
            )));
        }
        let s = s.unwrap_or_default().to_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

const SECRET_TOKENS: &[&str] = &[
    "secret",
    "token",
    "password",
    "cookie",
    "authorization",
    "api_key",
    "apikey",
    "private_key",
    "credential",
];

/// Bounded scalar facts with provenance.
pub fn facts(value: Option<&Value>) -> ProjectsResult<Vec<Value>> {
    let Some(list) = value else {
        return Ok(Vec::new());
    };
    let items = list.as_array().ok_or_else(|| {
        crate::error::value("facts must be a list of at most 100 provenance-backed values")
    })?;
    if items.len() > 100 {
        return Err(crate::error::value(
            "facts must be a list of at most 100 provenance-backed values",
        ));
    }
    let mut result = Vec::new();
    for item in items {
        let object = item.as_object().ok_or_else(|| {
            crate::error::value("each fact requires name, value, provenance, and observed_at")
        })?;
        if object.len() != 4
            || !object.contains_key("name")
            || !object.contains_key("value")
            || !object.contains_key("provenance")
            || !object.contains_key("observed_at")
        {
            return Err(crate::error::value(
                "each fact requires name, value, provenance, and observed_at",
            ));
        }
        let name = &object["name"];
        let fact = &object["value"];
        let provenance = &object["provenance"];
        let observed_at = &object["observed_at"];
        let fact_scalar_ok = match fact {
            Value::String(s) => s.chars().count() <= 2048,
            Value::Number(n) => n.as_f64().is_none_or(f64::is_finite),
            Value::Bool(_) => true,
            _ => false,
        };
        if name.as_str().is_none_or(|n| !fact_name_re().is_match(n))
            || provenance
                .as_str()
                .is_none_or(|p| p.is_empty() || p.len() > 512)
            || !fact_scalar_ok
        {
            return Err(crate::error::value(
                "fact fields must be bounded scalar values with provenance",
            ));
        }
        if !observed_at.is_null() {
            let at = observed_at.as_str().ok_or_else(|| {
                crate::error::value("fact observed_at must be RFC3339 UTC or null when unknown")
            })?;
            let parsed = pytime::fromisoformat(&at.replace('Z', "+00:00")).map_err(|_| {
                crate::error::value("fact observed_at must be RFC3339 UTC or null when unknown")
            })?;
            if !parsed.is_utc() {
                return Err(crate::error::value(
                    "fact observed_at must be RFC3339 UTC or null when unknown",
                ));
            }
        }
        let lower = name.as_str().unwrap_or_default().to_lowercase();
        if SECRET_TOKENS.iter().any(|t| lower.contains(t)) {
            return Err(crate::error::value(
                "facts must not store credentials or secrets",
            ));
        }
        result.push(json!({
            "name": name,
            "value": fact,
            "provenance": provenance,
            "observed_at": observed_at,
        }));
    }
    Ok(result)
}

fn document(
    target: &str,
    label: Option<&str>,
    facts: Vec<Value>,
    templates: Vec<String>,
    profiles: Vec<String>,
) -> ProjectsResult<Value> {
    Ok(json!({
        "format": PROJECT_FORMAT,
        "version": PROJECT_VERSION,
        "project_uuid": Uuid::new_v4().to_string(),
        "created_at": pytime::zulu(),
        "site": site(target, label)?,
        "facts": facts,
        "template_references": templates,
        "profile_references": profiles,
        "artifact_directories": {"scans": "scans", "reports": "reports", "log": "log.md"},
    }))
}

/// Exclusively create a file with mode 0o600 and fsync it.
fn write_new(path: &Path, content: &str) -> ProjectsResult<()> {
    use std::io::Write;
    let mut file = io(std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(path))?;
    io(file.write_all(content.as_bytes()))?;
    io(file.sync_all())
}

/// Publish the manifest via stage and hard-link; never replaces an existing manifest.
fn publish_manifest(project: &Path, document: &Value) -> ProjectsResult<()> {
    let staged = project.join(".project.json.stage");
    write_new(&staged, &(crate::pyjson::dumps_indent2(document) + "\n"))?;
    let linked = fs::hard_link(&staged, project.join("project.json"));
    let cleanup = fs::remove_file(&staged);
    let _ = cleanup;
    linked.map_err(ProjectsError::Io)?;
    locks::fsync_directory(project).map_err(ProjectsError::Io)
}

/// `create_project`: one new project directory; existing paths are never replaced.
pub fn create_project(
    directory: &Path,
    target: &str,
    label: Option<&str>,
    facts: Option<&Value>,
    template_references: Option<&Value>,
    profile_references: Option<&Value>,
) -> ProjectsResult<Value> {
    let root = directory;
    if root.components().any(|c| c.as_os_str() == "..") {
        return Err(crate::error::value(
            "project directory must not contain parent traversal",
        ));
    }
    let parent = self::directory(
        root.parent().unwrap_or_else(|| Path::new("")),
        "project parent",
    )?;
    if root.file_name().is_none()
        || matches!(
            root.file_name().and_then(|n| n.to_str()),
            Some(".") | Some("..")
        )
        || std::fs::symlink_metadata(root).is_ok()
    {
        return Err(crate::error::value(
            "project directory already exists or is not a new path",
        ));
    }
    let target = self::target(target)?;
    let document = document(
        &target,
        label,
        self::facts(facts)?,
        references(template_references, "template_references")?,
        references(profile_references, "profile_references")?,
    )?;
    let project = parent.join(root.file_name().unwrap_or_default());
    io(fs::create_dir(&project))?;
    set_dir_mode(&project)?;
    for name in ["scans", "reports"] {
        let child = project.join(name);
        io(fs::create_dir(&child))?;
        set_dir_mode(&child)?;
    }
    write_new(
        &project.join("log.md"),
        "# Project log\n\nProject created; no audit work has run.\n",
    )?;
    publish_manifest(&project, &document)?;
    open_project(&project, None)
}

fn set_dir_mode(path: &Path) -> ProjectsResult<()> {
    io(spiderhead_core::filesystem::set_permissions_mode(
        path, 0o700,
    ))
}

/// Validate the workspace and return `(root, document)`.
pub fn load(directory: &Path) -> ProjectsResult<(PathBuf, Value)> {
    let root = self::directory(directory, "project directory")?;
    for name in ["scans", "reports"] {
        let child = root.join(name);
        if child.is_symlink() || !child.is_dir() {
            return Err(crate::error::value(format!(
                "project {name} directory is missing or unsafe"
            )));
        }
    }
    let log = root.join("log.md");
    if log.is_symlink() || !log.is_file() {
        return Err(crate::error::value("project log is missing or unsafe"));
    }
    let manifest = root.join("project.json");
    let meta = io(manifest.metadata())?;
    if manifest.is_symlink() || !manifest.is_file() || meta.len() > MAX_JSON_BYTES {
        return Err(crate::error::value(
            "project.json is missing, unsafe, or exceeds its byte limit",
        ));
    }
    let text = io(fs::read_to_string(&manifest))?;
    let document: Value = serde_json::from_str(&text)
        .map_err(|_| crate::error::value("project.json is not valid JSON"))?;
    let object = document
        .as_object()
        .ok_or_else(|| crate::error::value("project.json has an unsupported shape"))?;
    spiderhead_core::project_format::migrate(object.clone()).map_err(crate::error::value)?;
    let keys: std::collections::BTreeSet<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "schema_version")
        .collect();
    let expected: std::collections::BTreeSet<&str> = [
        "format",
        "version",
        "project_uuid",
        "created_at",
        "site",
        "facts",
        "template_references",
        "profile_references",
        "artifact_directories",
    ]
    .into_iter()
    .collect();
    if keys != expected {
        return Err(crate::error::value("project.json has an unsupported shape"));
    }
    if !matches(object["format"].as_str(), PROJECT_FORMAT)
        || object["version"].as_i64() != Some(PROJECT_VERSION)
    {
        return Err(crate::error::value(
            "project.json format or version is unsupported",
        ));
    }
    if object["project_uuid"].as_str().is_none() || object["created_at"].as_str().is_none() {
        return Err(crate::error::value(
            "project UUID or creation time is invalid",
        ));
    }
    if Uuid::parse_str(object["project_uuid"].as_str().unwrap_or_default()).is_err() {
        return Err(crate::error::value(
            "project UUID or creation time is invalid",
        ));
    }
    let created_at = pytime::fromisoformat(
        &object["created_at"]
            .as_str()
            .unwrap_or_default()
            .replace('Z', "+00:00"),
    )
    .map_err(|_| crate::error::value("project UUID or creation time is invalid"))?;
    if !created_at.is_utc() {
        return Err(crate::error::value(
            "project creation time must be RFC3339 UTC",
        ));
    }
    let raw_target = match object.get("site").and_then(|s| s.as_object()) {
        Some(site) => site
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        None => String::new(),
    };
    let target = self::target(&raw_target)
        .map_err(|_| crate::error::value("project site identity is invalid"))?;
    let site_value = object.get("site").cloned().unwrap_or(Value::Null);
    let label = site_value.get("label").and_then(Value::as_str);
    let expected_site = site(&target, label)
        .map_err(|_| crate::error::value("project site identity is invalid"))?;
    if site_value != expected_site {
        return Err(crate::error::value("project site identity is invalid"));
    }
    facts(Some(&object["facts"]))?;
    references(Some(&object["template_references"]), "template_references")?;
    references(Some(&object["profile_references"]), "profile_references")?;
    if object["artifact_directories"]
        != json!({"scans": "scans", "reports": "reports", "log": "log.md"})
    {
        return Err(crate::error::value(
            "project artifact references must remain portable relative paths",
        ));
    }
    Ok((root, document))
}

/// `open_project`: validated project + presentation + aggregate checklist.
pub fn open_project(directory: &Path, expected_site: Option<&str>) -> ProjectsResult<Value> {
    let (root, document) = load(directory)?;
    if let Some(expected) = expected_site {
        if normalize_domain(expected) != document["site"]["host"].as_str().unwrap_or_default() {
            return Err(crate::error::value(
                "project site identity conflicts with the requested site",
            ));
        }
    }
    let presentation = project_presentation(&root, &document)?;
    let checklist = crate::coverage::coverage_status(&root, true)?;
    let checklist =
        crate::runtime::aggregate_coverage(&root.to_string_lossy(), checklist, None, true)?;
    Ok(json!({
        "ok": true,
        "project": presentation,
        "path": root.to_string_lossy(),
        "checklist": checklist,
    }))
}

/// `spiderhead init`: create `<dir>/seo` as a project, with a `.gitignore` for the heavy data.
///
/// `site` defaults to the directory name when it looks like a host name.
pub fn init_project(dir: &Path, site: Option<&str>) -> ProjectsResult<Value> {
    let dir = self::directory(dir, "init directory")?;
    let target = match site {
        Some(s) if s.contains("://") => s.to_owned(),
        Some(s) => format!("https://{s}/"),
        None => {
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if !name.contains('.') {
                return Err(crate::error::value(
                    "pass --site URL: the directory name is not a host name",
                ));
            }
            format!("https://{name}/")
        }
    };
    let project = dir.join(spiderhead_core::project_root::PROJECT_DIR);
    let created = create_project(&project, &target, None, None, None, None)?;
    write_new(
        &project.join(".gitignore"),
        "scans/\nlogs/\n*.db\n*.sqlite\n*.sqlite-*\n",
    )?;
    Ok(created)
}

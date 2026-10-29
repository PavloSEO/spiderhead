//! for audit profiles. The full profile is a projection of this manifest:
//! every request has an exact CLI name and either a parsed export key, a
//! raw-evidence id, or both. The local help check is the capability source; an
//! issue being present in the analyzer registry does not make an unsupported
//! CLI export real.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use spiderhead_parse::py_str::py_repr;

use crate::loader::{discover_exports, read_table, LoadedExports};

/// A single requested export: a CLI name plus its parsed keys and/or raw id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    /// "tabs" | "bulk" | "reports".
    pub group: String,
    /// Exact CLI request name.
    pub name: String,
    /// Parsed logical keys this request feeds.
    pub keys: Vec<String>,
    /// Raw-evidence id for requests nothing parses into a table.
    pub raw_id: Option<String>,
    /// Filename tokens used to find the raw evidence file.
    pub file_tokens: Vec<String>,
    /// Logical keys derived from the first parsed key.
    pub derived_keys: Vec<String>,
    /// Columns the source table must carry for derivation to be possible.
    pub derived_required_columns: Vec<String>,
}

impl ExportRequest {
    fn new(group: &str, name: &str, keys: &[&str]) -> Self {
        Self {
            group: group.to_owned(),
            name: name.to_owned(),
            keys: keys.iter().map(|s| s.to_string()).collect(),
            raw_id: None,
            file_tokens: Vec::new(),
            derived_keys: Vec::new(),
            derived_required_columns: Vec::new(),
        }
    }

    fn raw(group: &str, name: &str, raw_id: &str, file_tokens: &[&str]) -> Self {
        Self {
            group: group.to_owned(),
            name: name.to_owned(),
            keys: Vec::new(),
            raw_id: Some(raw_id.to_owned()),
            file_tokens: file_tokens.iter().map(|s| s.to_string()).collect(),
            derived_keys: Vec::new(),
            derived_required_columns: Vec::new(),
        }
    }

    fn sitemap_in() -> Self {
        Self {
            group: "tabs".to_owned(),
            name: "Sitemaps:URLs in Sitemap".to_owned(),
            keys: vec!["sitemap_in".to_owned()],
            raw_id: None,
            file_tokens: Vec::new(),
            derived_keys: vec!["sitemap_redirects".to_owned(), "sitemap_non_200".to_owned()],
            derived_required_columns: vec!["Status Code".to_owned()],
        }
    }
}

/// `FULL_EXPORT_MANIFEST`: the full-profile request list.
pub fn full_export_manifest() -> &'static [ExportRequest] {
    use std::sync::OnceLock;
    static M: OnceLock<Vec<ExportRequest>> = OnceLock::new();
    M.get_or_init(|| {
        vec![
            ExportRequest::new("tabs", "Internal:All", &["internal_all"]),
            ExportRequest::new("tabs", "Response Codes:Client Error (4xx)", &["resp_4xx"]),
            ExportRequest::new("tabs", "Response Codes:Server Error (5xx)", &["resp_5xx"]),
            ExportRequest::new("tabs", "Response Codes:Redirection (3xx)", &["resp_3xx"]),
            ExportRequest::sitemap_in(),
            ExportRequest::new("tabs", "Sitemaps:URLs not in Sitemap", &["sitemap_not_in"]),
            ExportRequest::new("tabs", "Sitemaps:Orphan URLs", &["sitemap_orphan"]),
            ExportRequest::new(
                "tabs",
                "Sitemaps:Non-Indexable URLs in Sitemap",
                &["sitemap_non_indexable"],
            ),
            ExportRequest::new("tabs", "Page Titles:Multiple", &["titles_multiple"]),
            ExportRequest::raw(
                "tabs",
                "Structured Data:Validation Errors",
                "structured_data_validation_errors",
                &["structured", "data", "validation", "errors"],
            ),
            ExportRequest::raw(
                "tabs",
                "Structured Data:Validation Warnings",
                "structured_data_validation_warnings",
                &["structured", "data", "validation", "warnings"],
            ),
            ExportRequest::new("tabs", "Security:Mixed Content", &["security_mixed"]),
            ExportRequest::new("tabs", "Images:Missing Alt Text", &["images_missing_alt"]),
            ExportRequest::new(
                "tabs",
                "Images:Missing Size Attributes",
                &["images_missing_size"],
            ),
            ExportRequest::new(
                "bulk",
                "Response Codes:Internal & External:Client Error (4xx) Inlinks",
                &["inlinks_4xx"],
            ),
            ExportRequest::new(
                "bulk",
                "Response Codes:Internal & External:Server Error (5xx) Inlinks",
                &["inlinks_5xx"],
            ),
            ExportRequest::new(
                "bulk",
                "Response Codes:Internal & External:Redirection (3xx) Inlinks",
                &["inlinks_3xx"],
            ),
            ExportRequest::new("reports", "Redirects:Redirect Chains", &["redirect_chains"]),
            ExportRequest::raw(
                "tabs",
                "JavaScript:All",
                "javascript_all",
                &["javascript", "all"],
            ),
            ExportRequest::raw(
                "tabs",
                "Canonicals:All",
                "canonicals_all",
                &["canonicals", "all"],
            ),
            ExportRequest::raw("tabs", "H1:All", "h1_all", &["h1", "all"]),
            ExportRequest::new("tabs", "Hreflang:Non-200 hreflang URLs", &["hreflang"]),
            ExportRequest::new("reports", "Hreflang:All hreflang URLs", &["all_hreflang"]),
            ExportRequest::new("tabs", "Page Titles:Duplicate", &["titles_duplicate"]),
            ExportRequest::new("tabs", "Meta Description:Duplicate", &["desc_duplicate"]),
            ExportRequest::new("tabs", "Images:Over X KB", &["images_over_kb"]),
            ExportRequest::new("tabs", "Security:Missing HSTS Header", &["security_hsts"]),
            ExportRequest::new(
                "tabs",
                "Structured Data:Missing",
                &["structured_data_missing"],
            ),
            ExportRequest::new("tabs", "Response Codes:No Response", &["resp_no_response"]),
            ExportRequest::new(
                "tabs",
                "Response Codes:Blocked by Robots.txt",
                &["resp_blocked"],
            ),
        ]
    })
}

fn all_inlinks_request() -> ExportRequest {
    ExportRequest::new("bulk", "Links:All Inlinks", &["all_inlinks"])
}

/// The CLI help flag that lists the export names of one group.
pub fn cli_help_flag(group: &str) -> Option<&'static str> {
    match group {
        "tabs" => Some("export-tabs"),
        "bulk" => Some("bulk-export"),
        "reports" => Some("save-report"),
        _ => None,
    }
}

/// Errors of the manifest surface; the message text matches the oracle's
/// `str(exc)` exactly (the reference exception class travels in the variant).
#[derive(Debug, Error)]
pub enum ManifestError {
    /// `ValueError`.
    #[error("{0}")]
    Value(String),
    /// `RuntimeError` (incomplete export manifest).
    #[error("{0}")]
    Runtime(String),
    /// `NotADirectoryError`.
    #[error("{0}")]
    NotDirectory(String),
    /// Loader failure while checking a raw-evidence file (message inlined).
    #[error("{0}")]
    Loader(String),
}

fn value_err(msg: impl Into<String>) -> ManifestError {
    ManifestError::Value(msg.into())
}

/// `profile_exports(manifest)`: the CLI config dict, so the manifest owns the
/// full-profile request list.
pub fn profile_exports(manifest: &[ExportRequest]) -> Value {
    let mut exports = json!({"tabs": [], "bulk": [], "reports": [], "fetch_all_inlinks": false});
    for request in manifest {
        if let Some(names) = exports[request.group.as_str()].as_array_mut() {
            names.push(json!(request.name));
        }
    }
    exports
}

/// `profile_exports()` with the full manifest (the reference default argument).
pub fn default_profile_exports() -> Value {
    profile_exports(full_export_manifest())
}

/// `requests_from_config`: resolve configured requests to known keys; retain
/// custom CLI requests verbatim.
pub fn requests_from_config(exports: &Value) -> Result<Vec<ExportRequest>, ManifestError> {
    let mut by_name: HashMap<(String, String), ExportRequest> = HashMap::new();
    for request in full_export_manifest()
        .iter()
        .cloned()
        .chain(std::iter::once(all_inlinks_request()))
    {
        by_name.insert((request.group.clone(), request.name.clone()), request);
    }
    let mut requests: Vec<ExportRequest> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for group in ["tabs", "bulk", "reports"] {
        // `exports.get(group, [])`: a missing key defaults to the empty list,
        // but an explicit null (or any non-list) fails the isinstance check.
        let names = match exports.get(group) {
            None => &Vec::new(),
            Some(Value::Array(items)) => items,
            Some(_) => {
                return Err(value_err(format!(
                    "exports.{group} must be a list of non-empty strings"
                )))
            }
        };
        if !names
            .iter()
            .all(|n| n.as_str().is_some_and(|s| !s.is_empty()))
        {
            return Err(value_err(format!(
                "exports.{group} must be a list of non-empty strings"
            )));
        }
        for name in names {
            let name = name.as_str().unwrap_or_default();
            let identity = (group.to_owned(), name.to_owned());
            if seen.contains(&identity) {
                return Err(value_err(format!(
                    "duplicate Screaming Frog export request: {group}:{name}"
                )));
            }
            seen.insert(identity.clone());
            let request = by_name.get(&identity).cloned().unwrap_or_else(|| {
                let mut tokens: Vec<String> = Vec::new();
                for token in filename_tokens(name) {
                    if !tokens.contains(&token) {
                        tokens.push(token);
                    }
                }
                ExportRequest {
                    group: group.to_owned(),
                    name: name.to_owned(),
                    keys: Vec::new(),
                    raw_id: Some(format!("custom:{group}:{name}")),
                    file_tokens: tokens,
                    derived_keys: Vec::new(),
                    derived_required_columns: Vec::new(),
                }
            });
            requests.push(request);
        }
    }
    // `if exports.get("fetch_all_inlinks"):` — reference truthiness, not == true.
    if exports
        .get("fetch_all_inlinks")
        .is_some_and(crate::pyvalue::truthy)
    {
        let identity = ("bulk".to_owned(), "Links:All Inlinks".to_owned());
        if !seen.contains(&identity) {
            requests.push(by_name[&identity].clone());
        }
    }
    Ok(requests)
}

/// `is_full_manifest`: whether the config still requests every full-profile item.
pub fn is_full_manifest(exports: &Value) -> Result<bool, ManifestError> {
    let requested: HashSet<(String, String)> = requests_from_config(exports)?
        .iter()
        .map(|r| (r.group.clone(), r.name.clone()))
        .collect();
    Ok(full_export_manifest()
        .iter()
        .all(|r| requested.contains(&(r.group.clone(), r.name.clone()))))
}

/// `resolved_profile`: do not label a hand-pruned ``full`` request a full run.
pub fn resolved_profile(profile: &str, exports: &Value) -> Result<String, ManifestError> {
    if profile == "lite" {
        return Ok("lite".to_owned());
    }
    if profile == "full" && is_full_manifest(exports)? {
        return Ok("full".to_owned());
    }
    Ok("custom".to_owned())
}

/// reference `str.splitlines()`: the full break set, not just `\n`/`\r\n`.
fn splitlines(text: &str) -> Vec<&str> {
    let is_break = |c: char| {
        matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        )
    };
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap_or('\0');
        if is_break(c) {
            lines.push(&text[start..i]);
            i += c.len_utf8();
            if c == '\r' && bytes.get(i) == Some(&b'\n') {
                i += 1;
            }
            start = i;
        } else {
            i += c.len_utf8();
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `help_names`: parse only the names printed under the per-export help heading.
pub fn help_names(help_text: &str, group: &str) -> Result<BTreeSet<String>, ManifestError> {
    let option = cli_help_flag(group).unwrap_or(group);
    let marker = format!("The option '--{option}' supports the following arguments:");
    let lines = splitlines(help_text);
    let start = lines
        .iter()
        .position(|line| line.trim() == marker)
        .map(|i| i + 1)
        .ok_or_else(|| {
            value_err(format!(
                "Screaming Frog help omitted the --{option} capability list"
            ))
        })?;
    let mut names: Vec<String> = Vec::new();
    let date_head = regex::Regex::new(r"^\d{4}-\d\d-\d\d").expect("static regex");
    for line in &lines[start..] {
        let name = line.trim();
        if name.is_empty() {
            if !names.is_empty() {
                break;
            }
            continue;
        }
        if date_head.is_match(name) {
            continue;
        }
        names.push(name.to_owned());
    }
    if names.is_empty() {
        return Err(value_err(format!(
            "Screaming Frog help returned no names for --{option}"
        )));
    }
    Ok(names.into_iter().collect())
}

/// `unsupported_requests`: requested names absent from the discovered capabilities.
pub fn unsupported_requests<'a>(
    requests: &'a [ExportRequest],
    capabilities: &HashMap<String, BTreeSet<String>>,
) -> Vec<&'a ExportRequest> {
    requests
        .iter()
        .filter(|r| {
            !capabilities
                .get(&r.group)
                .is_some_and(|names| names.contains(&r.name))
        })
        .collect()
}

/// `output_tokens`: file tokens, or the conservative name-derived fallback.
pub fn output_tokens(request: &ExportRequest) -> Vec<String> {
    if !request.file_tokens.is_empty() {
        return request.file_tokens.clone();
    }
    let mut tokens: Vec<String> = Vec::new();
    for token in filename_tokens(&request.name) {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

/// `validate_export_files`: require every requested file in this fresh
/// directory, parsing raw-only evidence too.
pub fn validate_export_files(
    exports_dir: &Path,
    requests: &[ExportRequest],
    derive_sitemap_status: bool,
) -> Result<Value, ManifestError> {
    if !exports_dir.is_dir() {
        return Err(ManifestError::NotDirectory(format!(
            "Screaming Frog export directory not found: {}",
            exports_dir.display()
        )));
    }
    let by_key_list =
        discover_exports(exports_dir).map_err(|e| ManifestError::Loader(e.to_string()))?;
    let by_key: HashMap<&str, &Path> = by_key_list
        .iter()
        .map(|(k, p)| (k.as_str(), p.as_path()))
        .collect();
    let mut candidates: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(exports_dir).map_err(|e| ManifestError::Loader(e.to_string()))? {
        let name = entry
            .map_err(|e| ManifestError::Loader(e.to_string()))?
            .file_name()
            .to_string_lossy()
            .into_owned();
        let low = name.to_lowercase();
        if low.ends_with(".csv") || low.ends_with(".xls") || low.ends_with(".xlsx") {
            candidates.push(name);
        }
    }
    candidates.sort();
    let mut failures: Vec<String> = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    let mut derived: Map<String, Value> = Map::new();
    for request in requests {
        let mut files: BTreeSet<String> = BTreeSet::new();
        let missing_keys: Vec<&str> = request
            .keys
            .iter()
            .filter(|key| !by_key.contains_key(key.as_str()))
            .map(|s| s.as_str())
            .collect();
        for key in &request.keys {
            if let Some(path) = by_key.get(key.as_str()) {
                if let Some(name) = path.file_name() {
                    files.insert(name.to_string_lossy().into_owned());
                }
            }
        }
        if !missing_keys.is_empty() {
            failures.push(format!(
                "{} {}: missing loader key(s) {}",
                request.group,
                py_repr(&request.name),
                missing_keys.join(", ")
            ));
        }
        if let Some(raw_id) = &request.raw_id {
            let tokens = output_tokens(request);
            let matches: Vec<&String> = candidates
                .iter()
                .filter(|name| {
                    let norm = name.to_lowercase().replace(['-', ' '], "_");
                    tokens.iter().all(|t| norm.contains(t.as_str()))
                })
                .collect();
            if matches.len() != 1 {
                let reason = if matches.is_empty() {
                    "missing".to_owned()
                } else {
                    format!(
                        "ambiguous: {}",
                        matches
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                failures.push(format!(
                    "{} {} ({raw_id}): {reason}",
                    request.group,
                    py_repr(&request.name)
                ));
            } else {
                let matched = matches[0].clone();
                files.insert(matched.clone());
                if let Err(exc) = read_table(&exports_dir.join(&matched)) {
                    failures.push(format!(
                        "{} {} ({raw_id}): unreadable ({exc})",
                        request.group,
                        py_repr(&request.name)
                    ));
                }
            }
        }
        let derived_keys: &[String] = if derive_sitemap_status {
            &request.derived_keys
        } else {
            &[]
        };
        for key in derived_keys {
            derived.insert(
                key.clone(),
                json!(request.keys.first().cloned().unwrap_or_default()),
            );
        }
        rows.push(json!({
            "kind": request.group,
            "cli_name": request.name,
            "logical_keys": request.keys,
            "derived_keys": derived_keys,
            "derived_required_columns": if derived_keys.is_empty() {
                Vec::<String>::new()
            } else {
                request.derived_required_columns.clone()
            },
            "raw_id": request.raw_id,
            "files": files.into_iter().collect::<Vec<_>>(),
        }));
    }
    if !failures.is_empty() {
        return Err(ManifestError::Runtime(format!(
            "Screaming Frog export manifest is incomplete; audit analysis was not started: {}",
            failures.join("; ")
        )));
    }
    Ok(json!({"requests": rows, "derived_keys": derived}))
}

/// `validate_loaded_requests`: fail before rule execution when a requested
/// table could not be parsed or derived.
pub fn validate_loaded_requests(
    exports: &LoadedExports,
    requests: &[ExportRequest],
    derive_sitemap_status: bool,
) -> Result<(), ManifestError> {
    let mut missing: Vec<String> = Vec::new();
    for request in requests {
        let derived_keys: &[String] = if derive_sitemap_status {
            &request.derived_keys
        } else {
            &[]
        };
        for key in request.keys.iter().chain(derived_keys.iter()) {
            if exports.frames.contains_key(key) {
                continue;
            }
            let reason = match exports.derivation_errors.get(key) {
                Some(reason) => reason.clone(),
                None => exports
                    .missing
                    .iter()
                    .find(|item| item.as_str() == key || item.starts_with(&format!("{key} ")))
                    .cloned()
                    .unwrap_or_else(|| {
                        "file was present but the expected logical table was not loaded".to_owned()
                    }),
            };
            missing.push(format!(
                "{} {} -> {key}: {reason}",
                request.group,
                py_repr(&request.name)
            ));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ManifestError::Runtime(format!(
            "Screaming Frog export manifest is incomplete; audit analysis was not started: {}",
            missing.join("; ")
        )))
    }
}

/// `_filename_tokens`: the name parts as a conservative fallback for
/// unconsumed custom exports.
fn filename_tokens(name: &str) -> Vec<String> {
    regex::Regex::new(r"[A-Za-z0-9]+")
        .expect("static regex")
        .find_iter(name)
        .map(|m| m.as_str().to_lowercase())
        .filter(|t| t != "x")
        .collect()
}

/// `help_fingerprint`: stable capability identity that omits timestamps,
/// paths, and launcher logs.
pub fn help_fingerprint(capabilities: &HashMap<String, BTreeSet<String>>) -> String {
    let mut groups: Vec<&String> = capabilities.keys().collect();
    groups.sort();
    let mut lines: Vec<String> = Vec::new();
    for group in groups {
        for name in &capabilities[group] {
            lines.push(format!("{group}:{name}"));
        }
    }
    hex(&Sha256::digest(lines.join("\n").as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

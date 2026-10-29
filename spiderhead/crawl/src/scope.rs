//! URL scope decisions: host-internal scope with no extra patterns, segments,
//! extensions or media filters beyond the defaults.
//! Gates run in a fixed order. The recorded reason is the first gate that fails, so the order is part of the contract.

use serde_json::Value;
use spiderhead_core::crawl_settings;

/// What a scope check decided about one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The recorded decision reason string.
    pub reason: String,
}

/// The crawl scope: internal boundary plus the include/exclude lists.
#[derive(Clone)]
pub struct Scope {
    start_host: String,
    internal_registrable: bool,
    exclude_hosts: Vec<String>,
    exclude_patterns: Vec<fancy_regex::Regex>,
    include_patterns: Vec<fancy_regex::Regex>,
    start_folder_only: bool,
    start_folder: Option<String>,
    exclude_extensions: Vec<String>,
    include_extensions: Vec<String>,
    exclude_media_types: Vec<String>,
    include_media_types: Vec<String>,
}

impl Scope {
    /// Build the scope from the resolved crawl config (`scope.*` section).
    pub fn from_config(config: &Value) -> Result<Scope, String> {
        let flat = crawl_settings::flatten(config);
        let get_list = |key: &str| -> Vec<String> {
            flat.get(key)
                .cloned()
                .unwrap_or(Value::Null)
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let internal = flat
            .get("scope.internal")
            .and_then(Value::as_str)
            .unwrap_or("host")
            .to_string();
        let start_folder_only = flat
            .get("scope.start_folder_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let start_host = String::new(); // set by `set_start_url`
                                        // Filter values are normalized like the reference: extensions lose
                                        // their leading dot and lists fold case, so `".PDF"` means `pdf`.
        let extension_list = |key: &str| -> Vec<String> {
            get_list(key)
                .iter()
                .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
                .collect()
        };
        let scope = Scope {
            start_host,
            internal_registrable: internal == "registrable_domain",
            exclude_hosts: get_list("scope.exclude_hosts")
                .iter()
                .map(|host| host.to_ascii_lowercase().trim_start_matches('.').to_owned())
                .collect(),
            exclude_patterns: compile(&get_list("scope.exclude_patterns"))?,
            include_patterns: compile(&get_list("scope.include_patterns"))?,
            start_folder_only,
            start_folder: None,
            exclude_extensions: extension_list("scope.exclude_extensions"),
            include_extensions: extension_list("scope.include_extensions"),
            exclude_media_types: get_list("scope.exclude_media_types")
                .iter()
                .map(|media| media.to_ascii_lowercase())
                .collect(),
            include_media_types: get_list("scope.include_media_types")
                .iter()
                .map(|media| media.to_ascii_lowercase())
                .collect(),
        };
        Ok(scope)
    }

    /// Fix the start URL: its host (and folder, when `start_folder_only`) anchors scope.
    /// A folder-anchored scope with a root start has nothing to stay below —
    /// the config is refused by name, before any fetch.
    pub fn set_start_url(&mut self, url: &str) -> Result<(), String> {
        if let Ok(parsed) = url::Url::parse(url) {
            self.start_host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
            if self.start_folder_only {
                let path = parsed.path();
                let path = if path.is_empty() { "/" } else { path };
                let folder = match path.rfind('/') {
                    Some(index) => &path[..index + 1],
                    None => "/",
                };
                let folder = if folder.is_empty() { "/" } else { folder };
                if folder == "/" {
                    return Err(
                        "scope.start_folder_only requires a start URL with a non-root path: \
                         the host root has no folder to stay below"
                            .to_owned(),
                    );
                }
                self.start_folder = Some(folder.to_owned());
            }
        }
        Ok(())
    }

    /// Fix only the start host: a paused artifact's stored scope carries no
    /// folder rule, so resuming it must not anchor one either.
    pub fn set_start_host(&mut self, host: &str) {
        self.start_host = host.to_ascii_lowercase();
    }

    fn same_host(&self, host: &str) -> bool {
        if self.internal_registrable {
            let a = spiderhead_core::net::registrable_domain(&self.start_host);
            let b = spiderhead_core::net::registrable_domain(host);
            if a.is_empty() || b.is_empty() {
                host == self.start_host
            } else {
                a == b
            }
        } else {
            host == self.start_host
        }
    }

    /// The enqueue-time rejection, checked in gate order. `None` means in scope.
    pub fn rejection(&self, url: &str) -> Option<Rejection> {
        let parsed = url::Url::parse(url).ok()?;
        let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
        if !self.same_host(&host) {
            return Some(Rejection {
                reason: "outside_host".into(),
            });
        }
        // An excluded host hides its subdomains too.
        if self
            .exclude_hosts
            .iter()
            .any(|bad| host == *bad || host.ends_with(&format!(".{bad}")))
        {
            return Some(Rejection {
                reason: "excluded_host".into(),
            });
        }
        if let Some(reason) = pattern_rejection(&self.exclude_patterns, &self.include_patterns, url)
        {
            return Some(reason);
        }
        if let Some(folder) = &self.start_folder {
            let path = parsed.path();
            let bare = folder.trim_end_matches('/');
            if !(path == bare || path.starts_with(folder.as_str())) {
                return Some(Rejection {
                    reason: "outside_start_folder".into(),
                });
            }
        }
        if let Some(reason) = extension_rejection(
            parsed.path(),
            &self.exclude_extensions,
            &self.include_extensions,
        ) {
            return Some(reason);
        }
        None
    }

    /// The scope after one operator narrowing step: an exclude pattern is
    /// appended to the existing list, an include pattern REPLACES the
    /// include allowlist (always a narrowing, never an expansion).
    pub fn narrowed(
        &self,
        exclude_pattern: Option<&str>,
        include_pattern: Option<&str>,
    ) -> Result<Scope, String> {
        let mut narrowed = self.clone();
        if let Some(pattern) = exclude_pattern {
            narrowed
                .exclude_patterns
                .push(compile(&[pattern.to_string()])?.remove(0));
        }
        if let Some(pattern) = include_pattern {
            narrowed.include_patterns = compile(&[pattern.to_string()])?;
        }
        Ok(narrowed)
    }

    /// The post-fetch media-type rejection (`response_media_rejection`).
    /// Missing or malformed types stay eligible unless an allowlist makes a
    /// positive type match necessary.
    pub fn response_media_rejection(&self, content_type: &str) -> Option<String> {
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let (major, minor) = media_type.split_once('/').unwrap_or(("", ""));
        let malformed = media_type.matches('/').count() != 1
            || !is_media_token(major)
            || !is_media_token(minor);
        if media_type.is_empty() || malformed {
            return if self.include_media_types.is_empty() {
                None
            } else {
                Some("media_type_unavailable".into())
            };
        }
        let matches = |pattern: &str| -> bool {
            if let Some(prefix) = pattern.strip_suffix('*') {
                media_type.starts_with(prefix)
            } else {
                media_type == pattern
            }
        };
        if self.exclude_media_types.iter().any(|p| matches(p)) {
            return Some("excluded_by_media_type".into());
        }
        if !self.include_media_types.is_empty()
            && !self.include_media_types.iter().any(|p| matches(p))
        {
            return Some("not_included_by_media_type".into());
        }
        None
    }
}

/// `[a-z0-9!#$%&'+\-.^_`|~]+` — the token alphabet of a media-type half.
fn is_media_token(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|ch| {
            ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || matches!(
                    ch,
                    '!' | '#'
                        | '$'
                        | '%'
                        | '&'
                        | '\''
                        | '+'
                        | '-'
                        | '.'
                        | '^'
                        | '_'
                        | '`'
                        | '|'
                        | '~'
                )
        })
}

fn compile(patterns: &[String]) -> Result<Vec<fancy_regex::Regex>, String> {
    patterns
        .iter()
        .map(|p| fancy_regex::Regex::new(p).map_err(|e| format!("bad scope pattern {p:?}: {e}")))
        .collect()
}

fn pattern_rejection(
    exclude: &[fancy_regex::Regex],
    include: &[fancy_regex::Regex],
    url: &str,
) -> Option<Rejection> {
    if exclude.iter().any(|re| re.is_match(url).unwrap_or(false)) {
        return Some(Rejection {
            reason: "excluded_by_pattern".into(),
        });
    }
    if !include.is_empty() && !include.iter().any(|re| re.is_match(url).unwrap_or(false)) {
        return Some(Rejection {
            reason: "not_included_by_pattern".into(),
        });
    }
    None
}

fn extension_rejection(path: &str, exclude: &[String], include: &[String]) -> Option<Rejection> {
    // `PurePosixPath(path).suffix`: the last dot of the LAST segment, and a
    // leading dot is a hidden-file marker, not an extension.
    let segment = path.rsplit('/').next().unwrap_or("");
    let extension = segment
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, ext)| ext.to_ascii_lowercase());
    if let Some(ext) = &extension {
        if exclude.iter().any(|e| e == ext) {
            return Some(Rejection {
                reason: "excluded_by_extension".into(),
            });
        }
    }
    if !include.is_empty() {
        let included = extension
            .as_ref()
            .is_some_and(|ext| include.iter().any(|e| e == ext));
        if !included {
            return Some(Rejection {
                reason: "not_included_by_extension".into(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope_for(start: &str, config: Value) -> Scope {
        let mut scope = Scope::from_config(&config).unwrap();
        scope.set_start_url(start).expect("anchor");
        scope
    }

    #[test]
    fn default_scope_is_same_host() {
        let scope = scope_for(
            "http://127.0.0.1:18701/",
            json!({"scope": {"internal": "host"}}),
        );
        assert_eq!(scope.rejection("http://127.0.0.1:18701/a.html"), None);
        assert_eq!(
            scope.rejection("https://www.example.com/").unwrap().reason,
            "outside_host"
        );
    }

    #[test]
    fn start_folder_scope_rejects_siblings() {
        let scope = scope_for(
            "http://x/docs/index.html",
            json!({"scope": {"internal": "host", "start_folder_only": true}}),
        );
        assert_eq!(scope.rejection("http://x/docs/a"), None);
        assert_eq!(
            scope.rejection("http://x/other").unwrap().reason,
            "outside_start_folder"
        );
    }

    #[test]
    fn url_scope_precedes_suffix_filters_and_suffix_exclusions_win() {
        let scope = scope_for(
            "http://example.test/",
            json!({"scope": {
                "include_patterns": ["/blog/"],
                "include_extensions": ["pdf"],
                "exclude_extensions": [".PDF"],
            }}),
        );
        assert_eq!(
            scope
                .rejection("http://example.test/files/a.pdf")
                .unwrap()
                .reason,
            "not_included_by_pattern"
        );
        assert_eq!(
            scope
                .rejection("http://example.test/blog/a.PDF?download=1")
                .unwrap()
                .reason,
            "excluded_by_extension"
        );
        let scope = scope_for(
            "http://example.test/",
            json!({"scope": {"include_extensions": ["pdf"]}}),
        );
        assert_eq!(
            scope
                .rejection("http://example.test/.well-known")
                .unwrap()
                .reason,
            "not_included_by_extension"
        );
    }

    #[test]
    fn media_rules_are_independent_case_insensitive_and_parameter_aware() {
        let scope = scope_for(
            "http://example.test/",
            json!({"scope": {
                "include_media_types": ["text/*", "application/pdf"],
                "exclude_media_types": ["text/xml"],
            }}),
        );
        assert_eq!(
            scope.response_media_rejection("Text/HTML; Charset=UTF-8"),
            None
        );
        assert_eq!(
            scope.response_media_rejection("application/PDF; version=1.7"),
            None
        );
        assert_eq!(
            scope
                .response_media_rejection("text/xml; charset=utf-8")
                .unwrap(),
            "excluded_by_media_type"
        );
        assert_eq!(
            scope.response_media_rejection("image/png").unwrap(),
            "not_included_by_media_type"
        );
        assert_eq!(
            scope.response_media_rejection("").unwrap(),
            "media_type_unavailable"
        );
        assert_eq!(
            scope.response_media_rejection("not a media type").unwrap(),
            "media_type_unavailable"
        );
    }

    #[test]
    fn exclusion_only_media_filter_does_not_reject_missing_content_type() {
        let scope = scope_for(
            "http://example.test/",
            json!({"scope": {"exclude_media_types": ["application/pdf"]}}),
        );
        assert_eq!(scope.response_media_rejection(""), None);
        assert_eq!(
            scope.response_media_rejection("text/html; charset=utf-8"),
            None
        );
        assert_eq!(
            scope.response_media_rejection("application/pdf").unwrap(),
            "excluded_by_media_type"
        );
    }
}

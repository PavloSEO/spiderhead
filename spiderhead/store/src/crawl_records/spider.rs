//! Breadth-first link discovery — the part that makes this a crawler.

#![allow(clippy::type_complexity)]

use crate::error::StoreError;
use crate::ledger::urlparts::urlsplit;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Regex source.
pub const MEDIA_TYPE_TOKEN: &str = r"[a-z0-9!#$%&'+\-.^_`|~]+\Z";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LinkEdge {
    pub source: String,
    pub destination: String,
    pub anchor: String,
    pub nofollow: bool,
    pub position: String,
    pub rel: Vec<String>,
    pub target: String,
    pub raw_href: String,
    pub anchor_source: String,
}

/// Identity for de-duplication: fragment dropped, nothing else rewritten.
pub fn canonical_key(url: &str) -> String {
    let parts = crate::frontier::urlsplit(url);
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    crate::frontier::urlunsplit(&parts.scheme, &parts.netloc, path, &parts.query)
}

/// The request target for a frontier entry.
///
/// A fragment selects nothing on the server, so keeping it on the queued and
/// fetched URL made a fragment-only variant of a page its own PageRecord
/// (#194) even though the canonical key above already treats the two as one
/// identity for de-duplication. Unlike that helper the path is left exactly as
/// given -- this value is what actually gets requested, not merely compared --
/// which is precisely what ``urldefrag`` does, so it does the work.
pub fn strip_fragment(url: &str) -> String {
    crate::frontier::defrag(url).to_string()
}

/// One named segment: matches a URL by path prefix, exact host, or a regex over
/// the whole URL -- the three shapes a multilingual or multi-regional site uses
/// (path prefix, subdomain, or a wholly separate domain -- #358).
///
/// Evaluated in declaration order by ``Scope.segment_for`` -- first match wins,
/// so overlapping rules are predictable rather than accidental, exactly as #21
/// specifies for segment rules in general.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SegmentRule {
    pub name: String,
    pub prefix: String,
    pub host: String,
    pub pattern: Option<String>,
}

/// Find `pattern` anywhere in `haystack`, like an unanchored search.
///
/// Patterns stored in a scan config were validated by `checked_pattern` at
/// ingest; a value that still fails to compile (corrupt artifact) or errors at
/// match time is treated as not matching -- the call sites cannot return an
/// error, so the scope fails closed rather than panicking.
fn re_search(pattern: &str, haystack: &str) -> bool {
    fancy_regex::Regex::new(pattern)
        .ok()
        .and_then(|re| re.find(haystack).ok().flatten())
        .is_some()
}

fn media_token_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // MEDIA_TYPE_TOKEN with the end-of-input anchor spelled out as an explicit `$` anchor.
        regex::Regex::new(r"\A[a-z0-9!#$%&'+\-.^_`|~]+\z").expect("static pattern")
    })
}

/// Whole-string match of the media-type token.
fn media_token(text: &str) -> bool {
    media_token_regex().is_match(text)
}

/// Truthiness of a stored JSON config value.
fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// String list under `key`, keeping only string entries.
/// configured list; non-string entries could not have been written).
fn string_list(scope: &BTreeMap<String, Value>, key: &str) -> Vec<String> {
    scope
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `PurePosixPath(path).suffix`: the last dot-suffix of the final component.
fn posix_suffix(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or("");
    name.rfind('.')
        .filter(|&i| i > 0 && i + 1 < name.len())
        .map_or("", |i| &name[i..])
}

impl SegmentRule {
    pub fn matches(&self, url: &str, path: &str, host: &str) -> bool {
        if !self.prefix.is_empty() && path.starts_with(&self.prefix) {
            return true;
        }
        if !self.host.is_empty() && host == self.host {
            return true;
        }
        self.pattern.as_deref().is_some_and(|p| re_search(p, url))
    }
}

/// Which discovered URLs a crawl may fetch.
///
/// The seed is always fetched: a crawl whose own start URL is filtered out
/// would report an empty site rather than a configuration mistake. Everything
/// reached from it is tested here, and every rejection is counted in
/// ``SpiderResult.excluded`` under the rule that rejected it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Scope {
    pub internal: String,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub include_extensions: BTreeSet<String>,
    pub exclude_extensions: BTreeSet<String>,
    pub include_media_types: Vec<String>,
    pub exclude_media_types: Vec<String>,
    pub exclude_hosts: BTreeSet<String>,
    pub segments: Vec<SegmentRule>,
    pub segments_only: BTreeSet<String>,
    pub start_folder_only: bool,
    pub start_folder: String,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.to_owned())
}

/// The host part of `url`, lowercased; empty when there is none.
fn hostname(url: &str) -> String {
    urlsplit(url).hostname().unwrap_or_default().to_lowercase()
}

impl Scope {
    pub fn from_config(scope: Option<&BTreeMap<String, Value>>) -> Scope {
        let empty = BTreeMap::new();
        let scope = scope.unwrap_or(&empty);
        let internal = scope
            .get("internal")
            .and_then(Value::as_str)
            .unwrap_or("host")
            .to_owned();
        let lower_set = |key: &str, strip_dot: bool| -> BTreeSet<String> {
            string_list(scope, key)
                .into_iter()
                .map(|v| {
                    let v = if strip_dot {
                        v.trim_start_matches('.')
                    } else {
                        v.strip_prefix('.').unwrap_or(&v)
                    };
                    v.to_lowercase()
                })
                .collect()
        };
        let segments = scope
            .get("segments")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let map = item.as_object()?;
                        Some(SegmentRule {
                            name: map.get("name")?.as_str()?.to_owned(),
                            prefix: map
                                .get("prefix")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            host: map
                                .get("host")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_lowercase(),
                            pattern: map
                                .get("pattern")
                                .and_then(Value::as_str)
                                .filter(|p| !p.is_empty())
                                .map(str::to_owned),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Scope {
            internal,
            include_patterns: string_list(scope, "include_patterns"),
            exclude_patterns: string_list(scope, "exclude_patterns"),
            include_extensions: lower_set("include_extensions", false),
            exclude_extensions: lower_set("exclude_extensions", false),
            include_media_types: string_list(scope, "include_media_types")
                .into_iter()
                .map(|v| v.to_lowercase())
                .collect(),
            exclude_media_types: string_list(scope, "exclude_media_types")
                .into_iter()
                .map(|v| v.to_lowercase())
                .collect(),
            exclude_hosts: lower_set("exclude_hosts", true),
            segments,
            segments_only: string_list(scope, "segments_only").into_iter().collect(),
            start_folder_only: py_truthy(scope.get("start_folder_only")),
            start_folder: String::new(),
        }
    }

    /// The scope after one operator narrowing step (#1485).
    ///
    /// An exclude pattern is appended to the existing list -- everything it
    /// matches stops being fetched. An include pattern REPLACES the include
    /// allowlist: from this point only URLs matching it pass, which is always
    /// a narrowing of what will be fetched, never an expansion. Patterns are
    /// already validated by ``checked_pattern``; this rebuilds the compiled
    /// rules the crawl actually applies.
    pub fn narrowed(
        &self,
        exclude_pattern: Option<&str>,
        include_pattern: Option<&str>,
    ) -> Result<Scope, StoreError> {
        // Compile now: a bad pattern is an error here, not at match time.
        // the same point instead of at match time.
        for pattern in [exclude_pattern, include_pattern].into_iter().flatten() {
            fancy_regex::Regex::new(pattern).map_err(|e| {
                invalid(&format!(
                    "scope pattern is not a valid regular expression: {e}"
                ))
            })?;
        }
        let mut scope = self.clone();
        if let Some(pattern) = exclude_pattern {
            scope.exclude_patterns.push(pattern.to_owned());
        }
        if let Some(pattern) = include_pattern {
            scope.include_patterns = vec![pattern.to_owned()];
        }
        Ok(scope)
    }

    /// The one named segment this URL belongs to -- every URL gets exactly
    /// one, a declared segment or the built-in ``DEFAULT_SEGMENT`` (#358).
    pub fn segment_for(&self, url: &str) -> String {
        let parts = urlsplit(url);
        let host = parts.hostname().unwrap_or_default().to_lowercase();
        for rule in &self.segments {
            if rule.matches(url, &parts.path, &host) {
                return rule.name.clone();
            }
        }
        spiderhead_core::crawl_settings::DEFAULT_SEGMENT.to_owned()
    }

    pub fn is_internal(&self, url: &str, start_host: &str) -> bool {
        let host = hostname(url);
        if host.is_empty() {
            return false;
        }
        // A segment naming an exact host declares that host part of this crawl,
        // regardless of scope.internal (#358).
        if self
            .segments
            .iter()
            .any(|rule| !rule.host.is_empty() && host == rule.host)
        {
            return true;
        }
        if self.internal == "registrable_domain" {
            return spiderhead_core::net::registrable_domain(&host)
                == spiderhead_core::net::registrable_domain(start_host);
        }
        host == start_host
    }

    /// Bind the start-folder rule to this crawl's start URL (#1481).
    ///
    /// The folder is the directory the start URL sits in -- the parent path
    /// segment, not the URL itself: a crawl started at ``/blog/post`` stays
    /// within ``/blog/``, one started at ``/blog/`` stays within ``/blog/``
    /// too. A start URL at the host root has no folder to stay below, so the
    /// toggle is refused there rather than silently scoping to everything.
    pub fn anchored_to(&self, start_url: &str) -> Result<Scope, StoreError> {
        let path = {
            let p = urlsplit(start_url).path;
            if p.is_empty() {
                "/".to_owned()
            } else {
                p
            }
        };
        let folder = match path.rfind('/') {
            Some(i) => {
                let f = &path[..i + 1];
                if f.is_empty() {
                    "/"
                } else {
                    f
                }
            }
            None => "/",
        };
        if folder == "/" {
            return Err(invalid(
                "scope.start_folder_only requires a start URL with a non-root path: \
                 the host root has no folder to stay below",
            ));
        }
        let mut scope = self.clone();
        scope.start_folder = folder.to_owned();
        Ok(scope)
    }

    /// The rule that rejects this URL, or "" when it may be fetched.
    pub fn rejection(&self, url: &str, start_host: &str) -> String {
        if !self.is_internal(url, start_host) {
            return "outside_host".to_owned();
        }
        let host = hostname(url);
        if self
            .exclude_hosts
            .iter()
            .any(|bad| host == *bad || host.ends_with(&format!(".{bad}")))
        {
            return "excluded_host".to_owned();
        }
        if self
            .exclude_patterns
            .iter()
            .any(|pattern| re_search(pattern, url))
        {
            return "excluded_by_pattern".to_owned();
        }
        if !self.include_patterns.is_empty()
            && !self
                .include_patterns
                .iter()
                .any(|pattern| re_search(pattern, url))
        {
            return "not_included_by_pattern".to_owned();
        }
        if self.start_folder_only && !self.start_folder.is_empty() {
            let path = {
                let p = urlsplit(url).path;
                if p.is_empty() {
                    "/".to_owned()
                } else {
                    p
                }
            };
            let folder = self.start_folder.trim_end_matches('/');
            if !(path == folder || path.starts_with(&self.start_folder)) {
                return "outside_start_folder".to_owned();
            }
        }
        if !self.segments_only.is_empty() && !self.segments_only.contains(&self.segment_for(url)) {
            return "outside_segment".to_owned();
        }
        let suffix = posix_suffix(&urlsplit(url).path).to_owned();
        let extension = suffix.strip_prefix('.').unwrap_or("").to_lowercase();
        if !extension.is_empty() && self.exclude_extensions.contains(&extension) {
            return "excluded_by_extension".to_owned();
        }
        if !self.include_extensions.is_empty() && !self.include_extensions.contains(&extension) {
            return "not_included_by_extension".to_owned();
        }
        String::new()
    }

    /// Return a body-filter reason for a response Content-Type, if any.
    ///
    /// This is separate from ``rejection``: suffixes are checked before a
    /// request, while a response media type exists only after the server
    /// answers. Missing or malformed types remain eligible unless an
    /// allowlist makes a positive type match necessary.
    pub fn response_media_rejection(&self, content_type: &str) -> String {
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let fallback = || {
            if self.include_media_types.is_empty() {
                ""
            } else {
                "media_type_unavailable"
            }
        };
        if media_type.is_empty() || media_type.matches('/').count() != 1 {
            return fallback().to_owned();
        }
        let (major, minor) = media_type.split_once('/').unwrap_or(("", ""));
        if !media_token(major) || !media_token(minor) {
            return fallback().to_owned();
        }
        let matches = |pattern: &str| -> bool {
            match pattern.strip_suffix("/*") {
                Some(prefix) => media_type.starts_with(prefix),
                None => media_type == pattern,
            }
        };
        if self.exclude_media_types.iter().any(|p| matches(p)) {
            return "excluded_by_media_type".to_owned();
        }
        if !self.include_media_types.is_empty()
            && !self.include_media_types.iter().any(|p| matches(p))
        {
            return "not_included_by_media_type".to_owned();
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope_config(value: Value) -> BTreeMap<String, Value> {
        value
            .as_object()
            .expect("object")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    // Scope methods are reachable from url_history, link_context and link_queries;
    // none of them may panic.
    #[test]
    fn default_scope_fetches_internal_and_rejects_external() {
        let scope = Scope::from_config(None);
        assert!(scope.is_internal("https://example.com/a", "example.com"));
        assert_eq!(scope.rejection("https://example.com/a", "example.com"), "");
        assert_eq!(
            scope.rejection("https://other.example/a", "example.com"),
            "outside_host"
        );
        assert_eq!(scope.rejection("not a url", "example.com"), "outside_host");
    }

    #[test]
    fn pattern_extension_host_and_segment_rules_reject_in_order() {
        let config = scope_config(json!({
            "exclude_hosts": ["EXAMPLE.com"],
            "exclude_patterns": ["/private/"],
            "include_patterns": ["^https://example\\.com"],
            "exclude_extensions": [".PDF"],
            "include_extensions": ["html"],
            "segments": [{"name": "en", "prefix": "/en/", "host": "", "pattern": null}],
            "segments_only": ["en"],
        }));
        let scope = Scope::from_config(Some(&config));
        // Internal hosts are rejected first: a listed host under scope "host" is
        // only reachable when it IS the start host.
        assert_eq!(
            scope.rejection("https://example.com/x", "example.com"),
            "excluded_host"
        );
        assert_eq!(
            scope.rejection("https://bad.example.com/x", "example.com"),
            "outside_host"
        );
        let without_host = Scope::from_config(Some(&scope_config(json!({
            "exclude_patterns": ["/private/"],
            "include_patterns": ["^https://example\\.com"],
            "exclude_extensions": [".PDF"],
            "include_extensions": ["html"],
            "segments": [{"name": "en", "prefix": "/en/", "host": "", "pattern": null}],
            "segments_only": ["en"],
        }))));
        assert_eq!(
            without_host.rejection("https://example.com/private/x", "example.com"),
            "excluded_by_pattern"
        );
        assert_eq!(
            without_host.rejection("https://example.com/en/a.pdf", "example.com"),
            "excluded_by_extension"
        );
        // `/fr/` is allowed by the include patterns but outside the named segments.
        assert_eq!(
            without_host.rejection("https://example.com/fr/a", "example.com"),
            "outside_segment"
        );
        assert_eq!(
            without_host.rejection("https://example.com/en/a.html", "example.com"),
            ""
        );
        assert_eq!(without_host.segment_for("https://example.com/en/a"), "en");
        assert_eq!(
            without_host.segment_for("https://example.com/fr/a"),
            spiderhead_core::crawl_settings::DEFAULT_SEGMENT
        );
    }

    #[test]
    fn registrable_domain_scope_and_host_segments() {
        let config = scope_config(json!({
            "internal": "registrable_domain",
            "segments": [{"name": "blog", "prefix": "", "host": "blog.other.org", "pattern": null}],
        }));
        let scope = Scope::from_config(Some(&config));
        assert!(scope.is_internal("https://www.example.com/a", "example.com"));
        // A segment's exact host is internal regardless of scope.internal.
        assert!(scope.is_internal("https://blog.other.org/a", "example.com"));
    }

    #[test]
    fn narrowed_replaces_includes_and_appends_excludes() {
        let scope = Scope::from_config(None);
        let narrowed = scope
            .narrowed(Some("/tmp/"), Some("^https://example\\.com/a"))
            .expect("valid patterns");
        assert_eq!(narrowed.include_patterns, vec!["^https://example\\.com/a"]);
        assert_eq!(narrowed.exclude_patterns, vec!["/tmp/"]);
        assert!(scope.narrowed(Some("(["), None).is_err());
    }

    #[test]
    fn anchored_folder_scopes_paths_below_the_start_folder() {
        let config = scope_config(json!({"start_folder_only": true}));
        let scope = Scope::from_config(Some(&config));
        let anchored = scope
            .anchored_to("https://example.com/blog/post")
            .expect("folder");
        assert_eq!(anchored.start_folder, "/blog/");
        assert_eq!(
            anchored.rejection("https://example.com/blog/other", "example.com"),
            ""
        );
        assert_eq!(
            anchored.rejection("https://example.com/docs/a", "example.com"),
            "outside_start_folder"
        );
        assert!(scope.anchored_to("https://example.com/").is_err());
    }

    #[test]
    fn media_type_rejection_matches_reference() {
        let config = scope_config(json!({
            "include_media_types": ["text/*"],
            "exclude_media_types": ["text/css"],
        }));
        let scope = Scope::from_config(Some(&config));
        assert_eq!(
            scope.response_media_rejection("text/css; charset=utf-8"),
            "excluded_by_media_type"
        );
        assert_eq!(
            scope.response_media_rejection("image/png"),
            "not_included_by_media_type"
        );
        assert_eq!(
            scope.response_media_rejection("bogus"),
            "media_type_unavailable"
        );
        let open = Scope::from_config(None);
        assert_eq!(open.response_media_rejection(""), "");
        assert_eq!(open.response_media_rejection("text/html"), "");
    }
}

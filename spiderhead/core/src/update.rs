//! Release feed normalisation for the update check.
//!
//! Constants of the release feed live here.
//!
//! Network reads with consent, downloads, checksum hashing and state-file I/O belong to the fetch and
//! filesystem layers.
//!
//! The version grammar accepts ASCII digits only, and a version component that does not fit in
//! `u64` is treated as a non-version.

use serde_json::Value;
use thiserror::Error;

/// Version of this toolkit build, compared against the newest release.
pub const TOOLKIT_VERSION: &str = "3.2.0";
/// Format identifier of the persisted update-check result.
pub const UPDATE_FORMAT: &str = "spiderhead.update-check.v1";
/// Source repository of releases, as `owner/name`.
pub const REPOSITORY: &str = "PavloSEO/spiderhead";
/// Releases read from the feed at most.
pub const MAX_RELEASES: usize = 10;
/// Assets read per release at most.
pub const MAX_ASSETS: usize = 20;
/// Changelog characters kept per release.
pub const MAX_CHANGELOG_CHARS: usize = 4_000;
/// Byte limit of the release feed.
pub const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
/// Byte limit of the persisted state file.
pub const MAX_STATE_BYTES: usize = 512 * 1024;
/// Byte limit of a checksum file.
pub const MAX_CHECKSUM_BYTES: usize = 128 * 1024;
/// Default download ceiling.
pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 250 * 1024 * 1024;
/// Hard download ceiling.
pub const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// Age after which an automatic check runs again: one day.
pub const AUTO_CHECK_SECONDS: u64 = 24 * 60 * 60;

/// A release that passed every check, with its usable assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The tag as published, e.g. `v1.2.3`.
    pub tag: String,
    /// The tag without its leading `v`.
    pub version: String,
    /// The release page on github.com.
    pub url: String,
    /// Publication time as the feed wrote it.
    pub published_at: Option<String>,
    /// Changelog, cut to [`MAX_CHANGELOG_CHARS`].
    pub changelog: String,
    /// Assets whose host, path and name are bound to this tag.
    pub assets: Vec<Asset>,
}

/// One downloadable asset of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// File name, without any path separator.
    pub name: String,
    /// Download URL on github.com under this tag.
    pub url: String,
    /// Size in bytes, when the feed declared a non-negative integer.
    pub size: Option<u64>,
    /// Lowercase SHA-256 hex digest, when the feed declared a valid one.
    pub sha256: Option<String>,
}

/// Failure of the feed normalisation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UpdateError {
    /// The feed was not a JSON array.
    #[error("release feed must be a JSON array")]
    NotAnArray,
}

/// The `(major, minor, patch)` of a release tag, or `None` when the tag is not a version.
///
/// Accepts an optional `v`, three dot-separated numbers and an optional `-` or `+` suffix made of
/// letters, digits, `.` and `-`.
pub fn version_of(value: &str) -> Option<(u64, u64, u64)> {
    let body = value.strip_prefix('v').unwrap_or(value);
    let (core, suffix) = match body.find(['-', '+']) {
        Some(index) => (&body[..index], Some(&body[index + 1..])),
        None => (body, None),
    };
    if let Some(suffix) = suffix {
        let allowed = !suffix.is_empty()
            && suffix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !allowed {
            return None;
        }
    }
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut numbers = [0u64; 3];
    for (slot, part) in numbers.iter_mut().zip(parts) {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some((numbers[0], numbers[1], numbers[2]))
}

/// Split an absolute URL into scheme (lowercased), netloc, path, query and fragment, for the
/// URL shapes the feed carries.
struct Split<'a> {
    scheme: String,
    netloc: &'a str,
    path: &'a str,
    query: &'a str,
    fragment: &'a str,
}

fn split_url(url: &str) -> Split<'_> {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => (String::new(), url),
    };
    let netloc_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (netloc, after_netloc) = if url.contains("://") {
        (&rest[..netloc_end], &rest[netloc_end..])
    } else {
        ("", url)
    };
    let (before_fragment, fragment) = after_netloc.split_once('#').unwrap_or((after_netloc, ""));
    let (path, query) = before_fragment
        .split_once('?')
        .unwrap_or((before_fragment, ""));
    Split {
        scheme,
        netloc,
        path,
        query,
        fragment,
    }
}

/// The lowercased host of a netloc, without userinfo or port.
fn hostname_of(netloc: &str) -> String {
    let host_port = netloc.rsplit('@').next().unwrap_or("");
    host_port
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Percent-encode everything except unreserved bytes.
fn quote_all(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// One asset of a release, or `None` when its name, URL or host do not match the release.
pub fn asset_of(item: &Value, tag: &str) -> Option<Asset> {
    let object = item.as_object()?;
    let name = object.get("name")?.as_str()?;
    let url = object.get("browser_download_url")?.as_str()?;
    if name.is_empty()
        || name.chars().count() > 255
        || name.contains(['/', '\\'])
        || url.chars().count() > 512
    {
        return None;
    }
    let parts = split_url(url);
    let expected_path = format!("/{REPOSITORY}/releases/download/{tag}/{}", quote_all(name));
    if parts.scheme != "https"
        || !matches!(parts.netloc, "github.com" | "github.com:443")
        || !parts.query.is_empty()
        || !parts.fragment.is_empty()
        || parts.path != expected_path
    {
        return None;
    }
    let sha256 = object
        .get("digest")
        .and_then(Value::as_str)
        .map(|digest| digest.strip_prefix("sha256:").unwrap_or(digest))
        .filter(|digest| is_sha256_hex(digest))
        .map(str::to_owned);
    let size = object.get("size").and_then(Value::as_u64);
    Some(Asset {
        name: name.to_owned(),
        url: url.to_owned(),
        size,
        sha256,
    })
}

/// Normalise a release feed: at most `limit` entries, drafts and pre-releases removed, each
/// release bound to its github.com tag page, newest version first.
pub fn normalize_releases(payload: &Value, limit: usize) -> Result<Vec<Release>, UpdateError> {
    let items = payload.as_array().ok_or(UpdateError::NotAnArray)?;
    let mut releases: Vec<((u64, u64, u64), Release)> = Vec::new();
    for item in items.iter().take(limit) {
        let Some(object) = item.as_object() else {
            continue;
        };
        if object.get("draft") == Some(&Value::Bool(true))
            || object.get("prerelease") == Some(&Value::Bool(true))
        {
            continue;
        }
        let Some(tag) = object.get("tag_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(parsed) = version_of(tag) else {
            continue;
        };
        let Some(html_url) = object.get("html_url").and_then(Value::as_str) else {
            continue;
        };
        let parts = split_url(html_url);
        if parts.scheme != "https"
            || hostname_of(parts.netloc) != "github.com"
            || parts.path != format!("/{REPOSITORY}/releases/tag/{tag}")
        {
            continue;
        }
        let assets = object
            .get("assets")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .take(MAX_ASSETS)
                    .filter_map(|raw| asset_of(raw, tag))
                    .collect()
            })
            .unwrap_or_default();
        let changelog = match object.get("body") {
            Some(Value::String(body)) => body.chars().take(MAX_CHANGELOG_CHARS).collect(),
            _ => String::new(),
        };
        releases.push((
            parsed,
            Release {
                tag: tag.to_owned(),
                version: tag.strip_prefix('v').unwrap_or(tag).to_owned(),
                url: html_url.to_owned(),
                published_at: object
                    .get("published_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                changelog,
                assets,
            },
        ));
    }
    releases.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    Ok(releases.into_iter().map(|(_, release)| release).collect())
}

/// Whether a parsed state file has the shape the update check writes.
pub fn state_shape_is_valid(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("format").and_then(Value::as_str) != Some(UPDATE_FORMAT)
        || object.get("checked_at").and_then(Value::as_str).is_none()
    {
        return false;
    }
    let Some(result) = object.get("result").and_then(Value::as_object) else {
        return false;
    };
    match result.get("releases") {
        None | Some(Value::Null) | Some(Value::Array(_)) => true,
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release_entry(tag: &str, body: &str, assets: Value) -> Value {
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "html_url": format!("https://github.com/PavloSEO/spiderhead/releases/tag/{tag}"),
            "published_at": "2026-10-12T12:00:00Z",
            "body": body,
            "assets": assets,
        })
    }

    #[test]
    fn release_feed_is_bounded_and_assets_are_host_and_tag_bound() {
        let digest = "a".repeat(64);
        let payload = json!([release_entry(
            "v9.9.9",
            &"x".repeat(MAX_CHANGELOG_CHARS + 10),
            json!([
                {
                    "name": "seohead_seotools-9.9.9-py3-none-any.whl",
                    "size": 10,
                    "digest": format!("sha256:{digest}"),
                    "browser_download_url": "https://github.com/PavloSEO/spiderhead/releases/download/v9.9.9/seohead_seotools-9.9.9-py3-none-any.whl",
                },
                {
                    "name": "malicious.whl",
                    "size": 10,
                    "digest": format!("sha256:{digest}"),
                    "browser_download_url": "https://attacker.example/malicious.whl",
                },
            ]),
        )]);
        let result = normalize_releases(&payload, 1).unwrap();
        assert_eq!(result[0].tag, "v9.9.9");
        assert_eq!(result[0].version, "9.9.9");
        assert_eq!(result[0].changelog.chars().count(), MAX_CHANGELOG_CHARS);
        let names: Vec<&str> = result[0].assets.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["seohead_seotools-9.9.9-py3-none-any.whl"]);
        assert_eq!(result[0].assets[0].sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(result[0].assets[0].size, Some(10));
    }

    // Behaviour of the feed normalisation: ordering, drafts and the feed type.
    #[test]
    fn drafts_prereleases_and_non_versions_are_dropped_and_newest_comes_first() {
        let payload = json!([
            release_entry("v1.2.0", "", json!([])),
            {"tag_name": "v9.0.0", "draft": true, "html_url": "https://github.com/PavloSEO/spiderhead/releases/tag/v9.0.0"},
            {"tag_name": "v8.0.0", "prerelease": true, "html_url": "https://github.com/PavloSEO/spiderhead/releases/tag/v8.0.0"},
            release_entry("nightly", "", json!([])),
            release_entry("v1.10.0", "", json!([])),
        ]);
        let tags: Vec<String> = normalize_releases(&payload, 10)
            .unwrap()
            .into_iter()
            .map(|release| release.tag)
            .collect();
        assert_eq!(tags, vec!["v1.10.0", "v1.2.0"]);
        assert_eq!(
            normalize_releases(&json!({"not": "a list"}), 10),
            Err(UpdateError::NotAnArray)
        );
    }

    // The limit applies before filtering.
    #[test]
    fn the_limit_is_applied_to_the_raw_feed() {
        let payload = json!([
            release_entry("v1.0.0", "", json!([])),
            release_entry("v2.0.0", "", json!([])),
        ]);
        let only_first = normalize_releases(&payload, 1).unwrap();
        assert_eq!(only_first.len(), 1);
        assert_eq!(only_first[0].tag, "v1.0.0");
    }

    // Version grammar cases.
    #[test]
    fn version_grammar_matches_the_reference_pattern() {
        assert_eq!(version_of("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(version_of("10.0.0-rc.1"), Some((10, 0, 0)));
        assert_eq!(version_of("1.2.3+build.7"), Some((1, 2, 3)));
        assert_eq!(version_of("vv1.2.3"), None);
        assert_eq!(version_of("1.2"), None);
        assert_eq!(version_of("1.2.3-"), None);
        assert_eq!(version_of("1.2.3 "), None);
        assert_eq!(version_of("1.2.x"), None);
    }

    // Asset rules: https only, github.com only, exact tag path, no separators, digest and size validated.
    #[test]
    fn asset_rules_reject_foreign_or_mismatched_downloads() {
        let good = "https://github.com/PavloSEO/spiderhead/releases/download/v1.0.0/app.zip";
        assert!(asset_of(
            &json!({"name": "app.zip", "browser_download_url": good}),
            "v1.0.0"
        )
        .is_some());
        for bad in [
            "http://github.com/PavloSEO/spiderhead/releases/download/v1.0.0/app.zip",
            "https://github.com.evil.example/PavloSEO/spiderhead/releases/download/v1.0.0/app.zip",
            "https://github.com/PavloSEO/spiderhead/releases/download/v2.0.0/app.zip",
            "https://github.com/PavloSEO/spiderhead/releases/download/v1.0.0/app.zip?x=1",
            "https://user@github.com/PavloSEO/spiderhead/releases/download/v1.0.0/app.zip",
        ] {
            let item = json!({"name": "app.zip", "browser_download_url": bad});
            assert!(asset_of(&item, "v1.0.0").is_none(), "accepted {bad}");
        }
        let separator = json!({"name": "a/b.zip", "browser_download_url": good});
        assert!(asset_of(&separator, "v1.0.0").is_none());
    }

    #[test]
    fn digest_and_size_are_optional_and_strictly_typed() {
        let url = "https://github.com/PavloSEO/spiderhead/releases/download/v1.0.0/app.zip";
        let base = json!({"name": "app.zip", "browser_download_url": url});
        let mut weak = base.clone();
        weak["digest"] = json!("sha256:XYZ");
        weak["size"] = json!(-4);
        let asset = asset_of(&weak, "v1.0.0").unwrap();
        assert_eq!(asset.sha256, None);
        assert_eq!(asset.size, None);
        let mut float_size = base;
        float_size["size"] = json!(12.0);
        assert_eq!(asset_of(&float_size, "v1.0.0").unwrap().size, None);
    }

    // State-shape rules of the state-file check.
    #[test]
    fn state_shape_requires_format_time_and_a_result_object() {
        let valid = json!({"format": UPDATE_FORMAT, "checked_at": "2026-10-27T00:00:00Z", "result": {"releases": []}});
        assert!(state_shape_is_valid(&valid));
        let no_format = json!({"checked_at": "x", "result": {}});
        assert!(!state_shape_is_valid(&no_format));
        let bad_releases =
            json!({"format": UPDATE_FORMAT, "checked_at": "x", "result": {"releases": "many"}});
        assert!(!state_shape_is_valid(&bad_releases));
    }
}

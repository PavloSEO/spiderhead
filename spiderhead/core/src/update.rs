//! Release feed normalisation for the update check, and the engine that installs a release.
//!
//! Constants of the release feed live here. The submodules hold the trust chain (`trust`),
//! resumable downloads (`download`), feed sources (`feed`) and installation (`install`); the
//! network itself is reached only through [`download::Fetch`], so everything is testable offline.
//! [`run`] ties them together without any JSON or CLI concerns.
//!
//! The version grammar accepts ASCII digits only, and a version component that does not fit in
//! `u64` is treated as a non-version.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

pub mod archive;
pub mod download;
pub mod feed;
pub mod install;
pub mod trust;

use download::{download_verified, fetch_small, Download, DownloadError, Fetch};
use install::{InstallError, InstallKind};
use trust::{parse_sums, verify_sums, SignaturePolicy, SignatureState, TrustError};

/// Version of this toolkit build, compared against the newest release.
pub const TOOLKIT_VERSION: &str = crate::user_agents::PRODUCT_VERSION;
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
    /// The local feed directory has no `download` folder.
    #[error("local feed has no download directory")]
    NoLocalFeed,
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

/// Transfer attempts per asset after transport failures.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// What one update run needs besides the releases and the network.
pub struct Options<'a> {
    /// Report the plan only.
    pub dry_run: bool,
    /// Largest acceptable asset.
    pub max_bytes: u64,
    /// Signature policy (normally [`SignaturePolicy::embedded`]).
    pub policy: SignaturePolicy,
    /// Per-user update directory (lock and download cache).
    pub updates_dir: &'a Path,
    /// How the running binary is installed.
    pub kind: InstallKind,
}

/// The result of one update run.
#[derive(Debug)]
pub enum Outcome {
    /// Nothing newer than the current version.
    Current {
        /// Newest release version.
        latest: String,
    },
    /// Dry run: what would be installed.
    Planned {
        /// Release version.
        version: String,
        /// Asset name.
        asset: String,
        /// File that would be replaced.
        target: PathBuf,
    },
    /// The new version is in place.
    Installed {
        /// Previous version.
        from: String,
        /// New version.
        to: String,
        /// Replaced file.
        target: PathBuf,
        /// SHA-256 of the installed file.
        sha256: String,
        /// Whether the release signature was checked.
        signature: SignatureState,
        /// The kept previous generation.
        backup: PathBuf,
        /// Processes that keep running the old file until restarted.
        restart: Vec<&'static str>,
    },
    /// A system package: the installer was downloaded and verified, and the user installs it.
    ManualInstall {
        /// Release version.
        version: String,
        /// The verified installer file.
        installer: PathBuf,
        /// How to install it (never run by this program).
        hint: String,
    },
    /// Nothing was changed; `state` is the stable machine-readable reason.
    Refused {
        /// Reason code.
        state: &'static str,
        /// Human explanation.
        reason: String,
    },
}

fn refused(state: &'static str, reason: impl Into<String>) -> Outcome {
    Outcome::Refused {
        state,
        reason: reason.into(),
    }
}

/// Check the newest of `releases` against `current` and, unless a dry run, download it through
/// `fetch`, verify it (signature if a key is embedded, then SHA-256) and install it.
/// `releases` must be ordered newest first.
pub fn run(releases: &[Release], current: &str, fetch: &dyn Fetch, opts: &Options) -> Outcome {
    let Some(latest) = releases.first() else {
        return refused("unavailable", "the feed has no usable stable release");
    };
    if !feed::is_newer(&latest.version, current) {
        return Outcome::Current {
            latest: latest.version.clone(),
        };
    }
    match &opts.kind {
        InstallKind::DevBuild => {
            return refused(
                "dev_build",
                "this is a development build; use --bin-dir to install into a directory",
            )
        }
        InstallKind::Unsupported(what) => {
            return refused(
                "unsupported_install",
                format!("updating an {what} installation is not supported yet"),
            )
        }
        _ => {}
    }
    let Some(name) = install::asset_for(&opts.kind, &latest.version) else {
        return refused(
            "unsupported_platform",
            "there is no release build for this platform",
        );
    };
    let find = |wanted: &str| latest.assets.iter().find(|a| a.name == wanted);
    let Some(asset) = find(&name) else {
        return refused(
            "unavailable",
            format!("release {} has no {name} asset", latest.tag),
        );
    };
    // Packages are only downloaded; their "target" is the cached installer.
    let target = install::target_of(&opts.kind)
        .unwrap_or_else(|| opts.updates_dir.join(&latest.tag).join(&name));
    if opts.dry_run {
        return Outcome::Planned {
            version: latest.version.clone(),
            asset: name,
            target,
        };
    }
    match install_release(latest, asset, fetch, opts) {
        Ok(Done::Replaced {
            sha256,
            signature,
            backup,
        }) => Outcome::Installed {
            from: current.to_owned(),
            to: latest.version.clone(),
            target,
            sha256,
            signature,
            backup,
            restart: vec!["spiderhead mcp"],
        },
        Ok(Done::Manual { installer, hint }) => Outcome::ManualInstall {
            version: latest.version.clone(),
            installer,
            hint,
        },
        Err((state, reason)) => Outcome::Refused { state, reason },
    }
}

/// What a successful install step produced.
enum Done {
    Replaced {
        sha256: String,
        signature: SignatureState,
        backup: PathBuf,
    },
    Manual {
        installer: PathBuf,
        hint: String,
    },
}

/// A refusal: reason code and explanation.
type Refusal = (&'static str, String);

fn no(state: &'static str, reason: impl Into<String>) -> Refusal {
    (state, reason.into())
}

fn install_release(
    release: &Release,
    asset: &Asset,
    fetch: &dyn Fetch,
    opts: &Options,
) -> Result<Done, Refusal> {
    let find = |wanted: &str| release.assets.iter().find(|a| a.name == wanted);
    let target = install::target_of(&opts.kind);
    if let Some(target) = &target {
        match install::prepare_stage(target) {
            Err(InstallError::NotWritable(dir)) => {
                return Err(no(
                    "not_writable",
                    format!("{} is not writable", dir.display()),
                ))
            }
            Err(other) => return Err(no("install_failed", other.to_string())),
            Ok(_) => {}
        }
    }
    let _lock = install::lock(opts.updates_dir).map_err(|e| match e {
        InstallError::Busy => no("busy", e.to_string()),
        other => no("error", other.to_string()),
    })?;
    // The checksum table is the authority; its signature (if a key is embedded) is checked first.
    let sums_asset = find("SHA256SUMS")
        .ok_or_else(|| no("checksum_unavailable", "the release has no SHA256SUMS"))?;
    let unreachable = |what: &str, e: download::FetchFailure| {
        no("error", format!("{what} could not be read: {e}"))
    };
    let sums_bytes = fetch_small(fetch, &sums_asset.url, MAX_CHECKSUM_BYTES)
        .map_err(|e| unreachable("SHA256SUMS", e))?;
    let sig = match (&opts.policy, find("SHA256SUMS.sig")) {
        (SignaturePolicy::Required(_), Some(sig)) => {
            Some(fetch_small(fetch, &sig.url, 4096).map_err(|e| unreachable("SHA256SUMS.sig", e))?)
        }
        _ => None,
    };
    let signature =
        verify_sums(&opts.policy, &sums_bytes, sig.as_deref()).map_err(|e| match e {
            TrustError::SignatureMissing => no("signature_missing", e.to_string()),
            other => no("signature_invalid", other.to_string()),
        })?;
    let sums = parse_sums(&sums_bytes).map_err(|e| no("checksum_unavailable", e.to_string()))?;
    let sha256 = sums
        .get(&asset.name)
        .ok_or_else(|| {
            no(
                "checksum_unavailable",
                TrustError::NoEntry(asset.name.clone()).to_string(),
            )
        })?
        .to_owned();
    if asset
        .sha256
        .as_deref()
        .is_some_and(|declared| declared != sha256)
    {
        return Err(no(
            "checksum_mismatch",
            "the feed digest disagrees with SHA256SUMS",
        ));
    }
    let cache = opts.updates_dir.join(&release.tag);
    let file = download_verified(
        fetch,
        &Download {
            url: &asset.url,
            name: &asset.name,
            sha256: &sha256,
            max_bytes: opts.max_bytes,
            dir: &cache,
        },
        DOWNLOAD_ATTEMPTS,
    )
    .map_err(|e| match e {
        DownloadError::ChecksumMismatch => no("checksum_mismatch", e.to_string()),
        DownloadError::SizeLimit => no("size_limit", e.to_string()),
        other => no("error", other.to_string()),
    })?;
    let version = release.version.clone();
    let replaced = match (&opts.kind, &target) {
        (InstallKind::Managed { installer }, _) => {
            // The cache is kept: the user runs this file.
            let hint = match *installer {
                "deb" => format!("sudo apt install {}", file.display()),
                _ => format!("open {}", file.display()),
            };
            return Ok(Done::Manual {
                installer: file,
                hint,
            });
        }
        (InstallKind::Prefix { profile, .. }, Some(target)) => {
            install_prefix(&file, profile, target, &version)?
        }
        (InstallKind::AppImage { .. }, Some(target)) => {
            // Starting an AppImage opens the window, so only the digest is checked.
            install::replace_file(&file, &sha256, target, &|_| Ok(())).map_err(replace_refusal)?
        }
        (_, Some(target)) => install::replace_file(&file, &sha256, target, &|exe| {
            install::probe_version(exe, &version)
        })
        .map_err(replace_refusal)?,
        (_, None) => return Err(no("unsupported_install", "nothing to replace")),
    };
    let _ = std::fs::remove_dir_all(&cache);
    Ok(Done::Replaced {
        sha256,
        signature,
        backup: replaced.backup,
    })
}

fn replace_refusal(e: InstallError) -> Refusal {
    match e {
        InstallError::RolledBack(_) => no("rolled_back", e.to_string()),
        InstallError::NotWritable(_) => no("not_writable", e.to_string()),
        other => no("install_failed", other.to_string()),
    }
}

/// Unpack a profile archive next to `target`, check it, and swap the `bin` directory.
fn install_prefix(
    archive: &Path,
    profile: &str,
    target: &Path,
    version: &str,
) -> Result<install::Replaced, Refusal> {
    let stage = install::prepare_stage(target).map_err(replace_refusal)?;
    let extracted = stage.join("archive.extract");
    let _ = std::fs::remove_dir_all(&extracted);
    let result = (|| {
        let root = archive::extract_tar_gz(archive, &extracted, profile)
            .and_then(|root| archive::verify_tree(&root).map(|()| root))
            .map_err(|e| no("install_failed", e.to_string()))?;
        install::replace_dir(&root.join("bin"), target, &|dir| {
            // A profile without the CLI (lite) is covered by the archive checksums alone.
            if dir.join("spiderhead").exists() {
                install::probe_version(&dir.join("spiderhead"), version)
            } else {
                Ok(())
            }
        })
        .map_err(replace_refusal)
    })();
    let _ = std::fs::remove_dir_all(&extracted);
    result
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
                    "name": "spiderhead-9.9.9.tar.gz",
                    "size": 10,
                    "digest": format!("sha256:{digest}"),
                    "browser_download_url": "https://github.com/PavloSEO/spiderhead/releases/download/v9.9.9/spiderhead-9.9.9.tar.gz",
                },
                {
                    "name": "malicious.tar.gz",
                    "size": 10,
                    "digest": format!("sha256:{digest}"),
                    "browser_download_url": "https://attacker.example/malicious.tar.gz",
                },
            ]),
        )]);
        let result = normalize_releases(&payload, 1).unwrap();
        assert_eq!(result[0].tag, "v9.9.9");
        assert_eq!(result[0].version, "9.9.9");
        assert_eq!(result[0].changelog.chars().count(), MAX_CHANGELOG_CHARS);
        let names: Vec<&str> = result[0].assets.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["spiderhead-9.9.9.tar.gz"]);
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

    #[cfg(unix)]
    #[test]
    fn a_signed_feed_is_required_once_a_key_exists() {
        use ring::rand::SystemRandom;
        use ring::signature::{Ed25519KeyPair, KeyPair};
        use sha2::{Digest, Sha256};

        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("key");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("pair");
        let policy = SignaturePolicy::Required(pair.public_key().as_ref().try_into().expect("key"));

        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("feed/download/v0.10.1");
        std::fs::create_dir_all(&dir).expect("feed");
        let binary = b"#!/bin/sh\necho spiderhead 0.10.1\n";
        let name = format!(
            "spiderhead-0.10.1-{}",
            install::target_triple().expect("triple")
        );
        std::fs::write(dir.join(&name), binary).expect("asset");
        let sums = format!("{:x}  {name}\n", Sha256::digest(binary));
        std::fs::write(dir.join("SHA256SUMS"), &sums).expect("sums");

        let target = tmp.path().join("bin");
        std::fs::create_dir_all(&target).expect("bin");
        let target = target.join("spiderhead");
        std::fs::write(&target, b"old").expect("old");
        let go = || {
            let releases = feed::list_local(&tmp.path().join("feed"), 5).expect("list");
            let opts = Options {
                dry_run: false,
                max_bytes: 1 << 20,
                policy,
                updates_dir: &tmp.path().join("updates"),
                kind: InstallKind::Standalone {
                    exe: target.clone(),
                },
            };
            run(&releases, "0.10.0", &download::FileFetch, &opts)
        };
        let state = |outcome: Outcome| match outcome {
            Outcome::Refused { state, .. } => state.to_owned(),
            Outcome::Installed { signature, .. } => signature.as_str().to_owned(),
            other => panic!("unexpected {other:?}"),
        };

        // No signature: refused, nothing changed.
        assert_eq!(state(go()), "signature_missing");
        // A signature over other bytes: refused.
        std::fs::write(
            dir.join("SHA256SUMS.sig"),
            pair.sign(b"something else").as_ref(),
        )
        .expect("sig");
        assert_eq!(state(go()), "signature_invalid");
        assert_eq!(std::fs::read(&target).expect("read"), b"old");
        // The right signature installs and reports it.
        std::fs::write(
            dir.join("SHA256SUMS.sig"),
            pair.sign(sums.as_bytes()).as_ref(),
        )
        .expect("sig");
        assert_eq!(state(go()), "verified");
        assert_eq!(std::fs::read(&target).expect("read"), binary);
    }

    #[test]
    fn a_package_install_is_downloaded_and_verified_but_never_installed() {
        use sha2::{Digest, Sha256};

        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("feed/download/v0.10.1");
        std::fs::create_dir_all(&dir).expect("feed");
        let kind = InstallKind::Managed { installer: "deb" };
        let name = install::asset_for(&kind, "0.10.1").expect("deb name");
        let package = b"not really a package";
        std::fs::write(dir.join(&name), package).expect("asset");
        let listed = |bytes: &[u8]| format!("{:x}  {name}\n", Sha256::digest(bytes));
        std::fs::write(dir.join("SHA256SUMS"), listed(package)).expect("sums");
        let go = || {
            let releases = feed::list_local(&tmp.path().join("feed"), 5).expect("list");
            let opts = Options {
                dry_run: false,
                max_bytes: 1 << 20,
                policy: SignaturePolicy::Disabled,
                updates_dir: &tmp.path().join("updates"),
                kind: kind.clone(),
            };
            run(&releases, "0.10.0", &download::FileFetch, &opts)
        };
        match go() {
            Outcome::ManualInstall {
                installer, hint, ..
            } => {
                assert_eq!(std::fs::read(&installer).expect("kept"), package);
                assert!(hint.starts_with("sudo apt install ") && hint.contains(&name));
            }
            other => panic!("unexpected {other:?}"),
        }
        // A package that does not match its checksum is refused and not kept.
        std::fs::write(dir.join("SHA256SUMS"), listed(b"other")).expect("sums");
        std::fs::remove_dir_all(tmp.path().join("updates")).expect("cache");
        assert!(matches!(
            go(),
            Outcome::Refused {
                state: "checksum_mismatch",
                ..
            }
        ));
    }
}

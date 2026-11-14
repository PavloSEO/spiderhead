//! Where releases come from: GitHub, or a local directory laid out like a GitHub release download
//! (`<root>/download/v<version>/<assets>`), plus the version the comparison starts from.

use std::path::{Path, PathBuf};

use super::{
    version_of, Asset, Release, UpdateError, MAX_ASSETS, MAX_CHANGELOG_CHARS, TOOLKIT_VERSION,
};
use crate::envvar::read_env;
use crate::host::Host;

/// The release feed in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedSource {
    /// The public GitHub releases.
    GitHub,
    /// A directory laid out as `download/<tag>/<assets>`.
    Local(PathBuf),
}

impl FeedSource {
    /// `--feed` beats `SPIDERHEAD_UPDATE_FEED` beats GitHub.
    pub fn resolve(flag: Option<&Path>, host: &dyn Host) -> Self {
        flag.map(Path::to_path_buf)
            .or_else(|| read_env(host, "UPDATE_FEED").map(PathBuf::from))
            .map_or(Self::GitHub, Self::Local)
    }
}

/// Releases of a local feed, newest version first, at most `limit`. Symbolic links are ignored.
pub fn list_local(root: &Path, limit: usize) -> Result<Vec<Release>, UpdateError> {
    let regular = |path: &Path, dir: bool| {
        std::fs::symlink_metadata(path).is_ok_and(|m| if dir { m.is_dir() } else { m.is_file() })
    };
    let downloads = root.join("download");
    let mut tags: Vec<((u64, u64, u64), String)> = std::fs::read_dir(&downloads)
        .map_err(|_| UpdateError::NoLocalFeed)?
        .flatten()
        .filter_map(|entry| {
            let tag = entry.file_name().into_string().ok()?;
            let parsed = tag.starts_with('v').then(|| version_of(&tag)).flatten()?;
            regular(&entry.path(), true).then_some((parsed, tag))
        })
        .collect();
    tags.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    Ok(tags
        .into_iter()
        .take(limit)
        .map(|(_, tag)| {
            let dir = downloads.join(&tag);
            let mut assets: Vec<Asset> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| regular(&entry.path(), false))
                .filter_map(|entry| {
                    Some(Asset {
                        name: entry.file_name().into_string().ok()?,
                        url: entry.path().to_string_lossy().into_owned(),
                        size: entry.metadata().ok().map(|m| m.len()),
                        sha256: None,
                    })
                })
                .collect();
            assets.sort_by(|a, b| a.name.cmp(&b.name));
            assets.truncate(MAX_ASSETS);
            let changelog = std::fs::read_to_string(dir.join("NOTES.md"))
                .map(|text| text.chars().take(MAX_CHANGELOG_CHARS).collect())
                .unwrap_or_default();
            Release {
                version: tag.trim_start_matches('v').to_owned(),
                url: format!("file://{}", dir.display()),
                tag,
                published_at: None,
                changelog,
                assets,
            }
        })
        .collect())
}

/// The version updates are compared against: `SPIDERHEAD_UPDATE_PRETEND_VERSION` when it is a valid
/// version, else this build's own.
pub fn current_version(host: &dyn Host) -> String {
    read_env(host, "UPDATE_PRETEND_VERSION")
        .map(|v| v.trim_start_matches('v').to_owned())
        .filter(|v| version_of(v).is_some())
        .unwrap_or_else(|| TOOLKIT_VERSION.to_owned())
}

/// Whether `candidate` is a strictly newer version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((version_of(candidate), version_of(current)), (Some(c), Some(k)) if c > k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;

    fn host(pairs: &[(&str, &str)]) -> FakeHost {
        FakeHost {
            env: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..FakeHost::default()
        }
    }

    #[test]
    fn local_feed_lists_tags_newest_first() {
        let tmp = tempfile::tempdir().expect("tmp");
        let download = tmp.path().join("download");
        for tag in ["v0.10.1", "v0.9.0", "nightly"] {
            std::fs::create_dir_all(download.join(tag)).expect("tag");
        }
        std::fs::write(download.join("v0.10.1/SHA256SUMS"), "x").expect("sums");
        std::fs::write(download.join("v0.10.1/NOTES.md"), "fixes").expect("notes");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hosts", download.join("v0.10.1/escape")).expect("link");
        let releases = list_local(tmp.path(), 10).expect("list");
        let versions: Vec<_> = releases.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, ["0.10.1", "0.9.0"]);
        assert_eq!(releases[0].changelog, "fixes");
        let names: Vec<_> = releases[0].assets.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["NOTES.md", "SHA256SUMS"]);
        assert_eq!(list_local(tmp.path(), 1).expect("limit").len(), 1);
    }

    #[test]
    fn pretend_version_makes_the_same_release_newer() {
        assert!(!is_newer(TOOLKIT_VERSION, &current_version(&host(&[]))));
        let pretend = host(&[("SPIDERHEAD_UPDATE_PRETEND_VERSION", "0.0.1")]);
        assert!(is_newer(TOOLKIT_VERSION, &current_version(&pretend)));
        let junk = host(&[("SPIDERHEAD_UPDATE_PRETEND_VERSION", "soon")]);
        assert_eq!(current_version(&junk), TOOLKIT_VERSION);
    }

    #[test]
    fn feed_flag_beats_env_beats_github() {
        let env = host(&[("SPIDERHEAD_UPDATE_FEED", "/from/env")]);
        assert_eq!(
            FeedSource::resolve(Some(Path::new("/flag")), &env),
            FeedSource::Local("/flag".into())
        );
        assert_eq!(
            FeedSource::resolve(None, &env),
            FeedSource::Local("/from/env".into())
        );
        assert_eq!(FeedSource::resolve(None, &host(&[])), FeedSource::GitHub);
    }
}

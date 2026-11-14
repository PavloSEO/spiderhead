//! Unpacking a release `.tar.gz` safely: the listing is checked before anything is written, only
//! regular files and directories under the expected top folder are accepted, and extraction goes
//! into a fresh private directory. The inner `SHA256SUMS` is then checked against the files.

use std::path::{Component, Path};
use std::process::Command;

use thiserror::Error;

use super::download::private_dir;
use super::trust::{parse_sums, sha256_file};

/// Why an archive was refused.
#[derive(Debug, Error)]
pub enum ArchiveError {
    /// The listing contains something that must not be extracted.
    #[error("unsafe archive: {0}")]
    Unsafe(String),
    /// The archive or its checksums do not hold together.
    #[error("{0}")]
    Invalid(String),
    /// `tar` could not run or failed.
    #[error("tar failed: {0}")]
    Tar(String),
    /// Local file trouble.
    #[error("archive storage error: {0}")]
    Io(#[from] std::io::Error),
}

/// Check a listing: `names` is `tar -tzf` output, `kinds` is `tar -tvzf` output (same entries,
/// first character is the entry type). Every entry must be a regular file or a directory (so no
/// link can point out of the tree) whose path is relative, free of `..`, and inside `top`.
pub fn check_listing(names: &str, kinds: &str, top: &str) -> Result<(), ArchiveError> {
    let unsafe_entry = |what: &str, name: &str| ArchiveError::Unsafe(format!("{what}: {name:?}"));
    let (names, kinds): (Vec<_>, Vec<_>) = (names.lines().collect(), kinds.lines().collect());
    if names.is_empty() || names.len() != kinds.len() {
        return Err(ArchiveError::Unsafe(
            "the listing is empty or inconsistent".into(),
        ));
    }
    for (name, kind) in names.iter().zip(&kinds) {
        if !matches!(kind.chars().next(), Some('-' | 'd')) {
            return Err(unsafe_entry("not a plain file or directory", name));
        }
        let path = Path::new(name);
        if name.is_empty() || name.contains('\0') || path.is_absolute() {
            return Err(unsafe_entry("absolute or empty path", name));
        }
        let mut parts = path
            .components()
            .filter(|c| !matches!(c, Component::CurDir));
        if parts.clone().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(unsafe_entry("path leaves the archive root", name));
        }
        if parts.next().is_none_or(|first| first.as_os_str() != top) {
            return Err(unsafe_entry("outside the profile folder", name));
        }
    }
    Ok(())
}

fn tar(args: &[&str], archive: &Path, extra: &[&Path]) -> Result<String, ArchiveError> {
    let out = Command::new("tar")
        .args(args)
        .arg(archive)
        .args(extra)
        .output()
        .map_err(|e| ArchiveError::Tar(e.to_string()))?;
    if !out.status.success() {
        return Err(ArchiveError::Tar(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Extract `archive` into `into` (created private, must not exist) after checking its listing;
/// returns `into/<top>`. Nothing is written when the listing is refused.
pub fn extract_tar_gz(
    archive: &Path,
    into: &Path,
    top: &str,
) -> Result<std::path::PathBuf, ArchiveError> {
    let names = tar(&["-tzf"], archive, &[])?;
    let kinds = tar(&["-tvzf"], archive, &[])?;
    check_listing(&names, &kinds, top)?;
    if into.exists() {
        return Err(ArchiveError::Invalid(format!(
            "{} already exists",
            into.display()
        )));
    }
    private_dir(into)?;
    let result = Command::new("tar")
        .args(["-xzf"])
        .arg(archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|e| ArchiveError::Tar(e.to_string()))
        .and_then(|out| {
            out.status.success().then_some(()).ok_or_else(|| {
                ArchiveError::Tar(String::from_utf8_lossy(&out.stderr).trim().to_owned())
            })
        });
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(into);
        return Err(error);
    }
    Ok(into.join(top))
}

/// Check every file listed in `<root>/SHA256SUMS` against its digest, and require a `bin/` folder.
pub fn verify_tree(root: &Path) -> Result<(), ArchiveError> {
    let bad = |m: String| ArchiveError::Invalid(m);
    let text = std::fs::read(root.join("SHA256SUMS"))
        .map_err(|_| bad("the archive has no SHA256SUMS".into()))?;
    let sums = parse_sums(&text).map_err(|e| bad(e.to_string()))?;
    let mut checked = 0;
    for (name, digest) in sums.entries() {
        let path = Path::new(name);
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(bad(format!("bad path in SHA256SUMS: {name}")));
        }
        let file = root.join(path);
        let regular = std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file());
        if !regular || sha256_file(&file)? != digest {
            return Err(bad(format!("{name} does not match the archive checksums")));
        }
        checked += 1;
    }
    if checked == 0 || !root.join("bin").is_dir() {
        return Err(bad("the archive has no binaries".into()));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A `.tar.gz` of `paths` (taken as written, `..` and absolute paths included).
    fn pack(dir: &Path, name: &str, cwd: &Path, paths: &[&str]) -> std::path::PathBuf {
        let out = dir.join(name);
        let status = Command::new("tar")
            .arg("-czPf")
            .arg(&out)
            .arg("-C")
            .arg(cwd)
            .args(paths)
            .status()
            .expect("tar");
        assert!(status.success());
        out
    }

    #[test]
    fn tar_listing_with_parent_dir_is_refused() {
        let tmp = tempfile::tempdir().expect("tmp");
        let work = tmp.path().join("work/inner");
        std::fs::create_dir_all(&work).expect("work");
        std::fs::write(tmp.path().join("work/x"), b"x").expect("x");
        std::fs::write(work.join("y"), b"y").expect("y");
        std::os::unix::fs::symlink("/etc/passwd", work.join("link")).expect("link");
        let abs = tmp.path().join("work/x");
        let into = tmp.path().join("out");

        let parent = pack(tmp.path(), "parent.tgz", &work, &["../x"]);
        let absolute = pack(tmp.path(), "abs.tgz", &work, &[abs.to_str().expect("utf8")]);
        let link = pack(
            tmp.path(),
            "link.tgz",
            tmp.path().join("work").as_path(),
            &["inner/link"],
        );
        for (archive, top) in [(&parent, "x"), (&absolute, "x"), (&link, "inner")] {
            let error = extract_tar_gz(archive, &into, top);
            assert!(
                matches!(error, Err(ArchiveError::Unsafe(_))),
                "{archive:?}: {error:?}"
            );
            assert!(!into.exists(), "nothing may be extracted");
        }
        assert!(!tmp.path().join("x").exists());

        // The listing rules, without tar: wrong top folder and inconsistent listings.
        assert!(check_listing("cli/bin/a\n", "-rw-r--r-- x\n", "cli").is_ok());
        assert!(check_listing("cli/../../a\n", "-rw x\n", "cli").is_err());
        assert!(check_listing("all/bin/a\n", "-rw x\n", "cli").is_err());
        assert!(check_listing("cli/a\ncli/b\n", "-rw x\n", "cli").is_err());
    }

    #[test]
    fn a_clean_archive_extracts_and_its_inner_sums_are_checked() {
        let tmp = tempfile::tempdir().expect("tmp");
        let top = tmp.path().join("src/cli");
        std::fs::create_dir_all(top.join("bin")).expect("bin");
        std::fs::write(top.join("bin/spiderhead"), b"binary").expect("bin file");
        let digest = sha256_file(&top.join("bin/spiderhead")).expect("sha");
        std::fs::write(
            top.join("SHA256SUMS"),
            format!("{digest}  bin/spiderhead\n"),
        )
        .expect("sums");
        let archive = pack(tmp.path(), "cli.tgz", &tmp.path().join("src"), &["cli"]);

        let root = extract_tar_gz(&archive, &tmp.path().join("out"), "cli").expect("extract");
        verify_tree(&root).expect("verified");
        assert!(extract_tar_gz(&archive, &tmp.path().join("out"), "cli").is_err());

        std::fs::write(root.join("bin/spiderhead"), b"tampered").expect("tamper");
        assert!(verify_tree(&root).is_err());
    }
}

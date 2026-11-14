//! Installing a verified binary: which file to replace, the per-user lock, an atomic replacement
//! that keeps the previous generation, the post-install probe with automatic rollback, and crash
//! recovery from a small journal.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};
use thiserror::Error;

use super::download::private_dir;
use super::trust::sha256_file;

/// The release-asset triple of this build, or `None` on a platform that has no release build.
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

/// How this binary was installed, which decides how it can be updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// A single CLI file (installer script, `cargo install`, `--bin-dir`): replaced in place.
    Standalone {
        /// The file to replace.
        exe: PathBuf,
    },
    /// A build inside a cargo `target` directory: never replaced.
    DevBuild,
    /// An installation that needs a different mechanism (package, bundle, AppImage).
    Unsupported(&'static str),
}

/// Classify the running binary. `exe` is the canonical path of the current executable.
pub fn detect(exe: &Path, appimage: bool) -> InstallKind {
    let parts: Vec<_> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    if parts
        .windows(2)
        .any(|w| w[0] == "target" && matches!(&*w[1], "debug" | "release"))
    {
        InstallKind::DevBuild
    } else if appimage {
        InstallKind::Unsupported("AppImage")
    } else if parts
        .windows(3)
        .any(|w| w[0].ends_with(".app") && w[1] == "Contents" && w[2] == "MacOS")
    {
        InstallKind::Unsupported("macOS application bundle")
    } else {
        InstallKind::Standalone {
            exe: exe.to_path_buf(),
        }
    }
}

/// The release asset that carries `version` for this install kind.
pub fn asset_for(kind: &InstallKind, version: &str) -> Option<String> {
    match kind {
        InstallKind::Standalone { .. } => {
            Some(format!("spiderhead-{version}-{}", target_triple()?))
        }
        _ => None,
    }
}

/// Failures of the install step.
#[derive(Debug, Error)]
pub enum InstallError {
    /// Another update holds the lock.
    #[error("another update is already running")]
    Busy,
    /// The directory of the target cannot be written (and no privileges are ever raised).
    #[error("{0} is not writable")]
    NotWritable(PathBuf),
    /// The new binary did not run or did not report the release version; nothing was replaced.
    #[error("{0}")]
    ProbeFailed(String),
    /// The replaced binary failed its probe and the previous one was restored.
    #[error("{0}")]
    RolledBack(String),
    /// There is no previous generation to restore.
    #[error("there is no previous version to restore")]
    NoBackup,
    /// Other file trouble.
    #[error("install storage error: {0}")]
    Io(#[from] std::io::Error),
}

/// The exclusive per-user update lock; released on drop.
#[derive(Debug)]
pub struct UpdateLock(#[allow(dead_code)] std::fs::File);

/// Take the lock in `updates_dir`, or `Busy` when another update holds it.
pub fn lock(updates_dir: &Path) -> Result<UpdateLock, InstallError> {
    private_dir(updates_dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(updates_dir.join(".lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(UpdateLock(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(InstallError::Busy),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// A successful replacement.
#[derive(Debug)]
pub struct Replaced {
    /// The file that now holds the new version.
    pub target: PathBuf,
    /// The previous version, kept for `rollback`.
    pub backup: PathBuf,
}

const STAGE: &str = ".spiderhead-update";

fn stage_dir(target: &Path) -> PathBuf {
    target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(STAGE)
}

fn file_name(target: &Path) -> String {
    target
        .file_name()
        .map_or_else(|| "spiderhead".into(), |n| n.to_string_lossy().into_owned())
}

fn backup_path(target: &Path) -> PathBuf {
    stage_dir(target).join(format!("{}.prev", file_name(target)))
}

/// Create the private staging directory next to `target` (same filesystem, so renames are atomic);
/// fails with `NotWritable` before anything is downloaded.
pub fn prepare_stage(target: &Path) -> Result<PathBuf, InstallError> {
    let stage = stage_dir(target);
    private_dir(&stage).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            InstallError::NotWritable(stage.parent().map(Path::to_path_buf).unwrap_or_default())
        } else {
            e.into()
        }
    })?;
    Ok(stage)
}

fn write_journal(stage: &Path, target: &Path, step: &str) -> std::io::Result<()> {
    std::fs::write(
        stage.join("journal.json"),
        json!({"target": target.to_string_lossy(), "step": step}).to_string(),
    )
}

/// `<exe> --version` must succeed and print `version`.
pub fn probe_version(exe: &Path, version: &str) -> Result<(), String> {
    let out = Command::new(exe)
        .arg("--version")
        .output()
        .map_err(|e| format!("the new binary does not run: {e}"))?;
    if out.status.success() && String::from_utf8_lossy(&out.stdout).contains(version) {
        Ok(())
    } else {
        Err(format!("the new binary does not report {version}"))
    }
}

/// Replace `target` with the verified file `src` (whose SHA-256 must be `sha256`): copy next to the
/// target, hash the copy, probe it, keep the old file as `.prev`, swap atomically, probe again and
/// restore the old file if that probe fails.
pub fn replace_file(
    src: &Path,
    sha256: &str,
    target: &Path,
    probe: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Replaced, InstallError> {
    let stage = prepare_stage(target)?;
    let staged = stage.join(format!("{}.new", file_name(target)));
    let backup = backup_path(target);
    let journal = stage.join("journal.json");
    // Older releases of this command staged beside the target.
    let _ = std::fs::remove_file(stage.with_file_name(".spiderhead-update-new"));
    write_journal(&stage, target, "staged")?;
    let abandon = || {
        let _ = std::fs::remove_file(&staged);
        let _ = std::fs::remove_file(&journal);
    };
    let staged_result = (|| -> Result<(), InstallError> {
        std::fs::copy(src, &staged)?;
        // The bytes that get installed are the bytes that were verified.
        if sha256_file(&staged)? != sha256 {
            return Err(InstallError::ProbeFailed(
                "the staged copy does not match its SHA-256".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        }
        probe(&staged).map_err(InstallError::ProbeFailed)
    })();
    if let Err(error) = staged_result {
        abandon();
        return Err(error);
    }
    let _ = std::fs::remove_file(&backup);
    if target.exists() && std::fs::hard_link(target, &backup).is_err() {
        std::fs::copy(target, &backup)?;
    }
    write_journal(&stage, target, "swapped")?;
    if let Err(error) = std::fs::rename(&staged, target) {
        abandon();
        return Err(error.into());
    }
    if let Err(reason) = probe(target) {
        if backup.exists() {
            std::fs::rename(&backup, target)?;
        }
        let _ = std::fs::remove_file(&journal);
        return Err(InstallError::RolledBack(reason));
    }
    let _ = std::fs::remove_file(&journal);
    Ok(Replaced {
        target: target.to_path_buf(),
        backup,
    })
}

/// Put the previous generation back; it is consumed, so a second rollback has nothing to restore.
pub fn rollback(target: &Path) -> Result<(), InstallError> {
    let backup = backup_path(target);
    if !backup.exists() {
        return Err(InstallError::NoBackup);
    }
    std::fs::rename(&backup, target)?;
    Ok(())
}

/// Finish or undo an update that was interrupted; the answer says what was done.
pub fn recover(target: &Path) -> Option<String> {
    let stage = stage_dir(target);
    let journal = stage.join("journal.json");
    let state: Value = serde_json::from_slice(&std::fs::read(&journal).ok()?).ok()?;
    let _ = std::fs::remove_file(&journal);
    let _ = std::fs::remove_file(stage.join(format!("{}.new", file_name(target))));
    if state["step"] != "swapped" {
        return Some("removed the leftovers of an interrupted update".into());
    }
    let runs = Command::new(target)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let backup = backup_path(target);
    if !runs && backup.exists() && std::fs::rename(&backup, target).is_ok() {
        return Some("restored the previous version after an interrupted update".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged_dir() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
        let tmp = tempfile::tempdir().expect("tmp");
        let target = tmp.path().join("spiderhead");
        std::fs::write(&target, b"old").expect("old");
        let src = tmp.path().join("download");
        std::fs::write(&src, b"new").expect("new");
        let sha = sha256_file(&src).expect("sha");
        (tmp, target, src, sha)
    }

    #[test]
    fn replace_keeps_a_backup_and_rolls_back_on_failed_probe() {
        let (_tmp, target, src, sha) = staged_dir();

        // A probe that fails before the swap leaves the target and the staging area untouched.
        let before = replace_file(&src, &sha, &target, &|_| Err("no".into()));
        assert!(matches!(before, Err(InstallError::ProbeFailed(_))));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(!stage_dir(&target).join("spiderhead.new").exists());

        // A probe that only fails once the new file is in place restores the old bytes.
        let calls = std::cell::Cell::new(0);
        let late = replace_file(&src, &sha, &target, &|_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err("crashes".into())
            } else {
                Ok(())
            }
        });
        assert!(matches!(late, Err(InstallError::RolledBack(_))));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");

        // A wrong digest never reaches the target.
        assert!(replace_file(&src, &"0".repeat(64), &target, &|_| Ok(())).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");

        // Success swaps and keeps the previous generation.
        let done = replace_file(&src, &sha, &target, &|_| Ok(())).expect("replace");
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(done.backup).unwrap(), b"old");
        assert!(!stage_dir(&target).join("journal.json").exists());
    }

    #[test]
    fn rollback_restores_the_previous_generation() {
        let (_tmp, target, src, sha) = staged_dir();
        assert!(matches!(rollback(&target), Err(InstallError::NoBackup)));
        replace_file(&src, &sha, &target, &|_| Ok(())).expect("replace");
        rollback(&target).expect("rollback");
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(matches!(rollback(&target), Err(InstallError::NoBackup)));
    }

    #[test]
    fn recover_restores_after_a_crash_mid_swap() {
        let (_tmp, target, _src, _sha) = staged_dir();
        let stage = prepare_stage(&target).expect("stage");
        // A crash after the swap left a binary that does not run, with the old one kept aside.
        std::fs::write(backup_path(&target), b"#!/bin/sh\nexit 0\n").expect("prev");
        std::fs::write(&target, b"garbage").expect("broken");
        write_journal(&stage, &target, "swapped").expect("journal");
        assert!(recover(&target).is_some());
        assert_eq!(std::fs::read(&target).unwrap(), b"#!/bin/sh\nexit 0\n");
        assert!(recover(&target).is_none());
    }

    #[test]
    fn second_updater_is_busy() {
        let tmp = tempfile::tempdir().expect("tmp");
        let first = lock(tmp.path()).expect("first");
        assert!(matches!(lock(tmp.path()), Err(InstallError::Busy)));
        drop(first);
        assert!(lock(tmp.path()).is_ok());
    }

    #[test]
    fn install_kind_follows_the_path() {
        let standalone = detect(Path::new("/home/u/.local/bin/spiderhead"), false);
        assert_eq!(
            standalone,
            InstallKind::Standalone {
                exe: "/home/u/.local/bin/spiderhead".into()
            }
        );
        assert_eq!(
            detect(Path::new("/w/repo/target/release/spiderhead"), false),
            InstallKind::DevBuild
        );
        let bundle = detect(
            Path::new("/x/SpiderHead.app/Contents/MacOS/spiderhead"),
            false,
        );
        assert!(matches!(bundle, InstallKind::Unsupported(_)));
        assert!(matches!(
            detect(Path::new("/tmp/.mount/usr/bin/spiderhead"), true),
            InstallKind::Unsupported(_)
        ));
    }
}

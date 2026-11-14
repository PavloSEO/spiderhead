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
    /// A Linux AppImage (`$APPIMAGE`): the file is replaced in place.
    AppImage {
        /// The `.AppImage` file.
        file: PathBuf,
    },
    /// A per-user `install.sh` layout (`<prefix>/lib/spiderhead/bin`) the user can write: the
    /// `bin` directory is replaced as a whole.
    Prefix {
        /// The install prefix.
        prefix: PathBuf,
        /// Profile id from `share/spiderhead/profile.toml`.
        profile: String,
    },
    /// A system package (`deb`, `pkg`) in a directory the user cannot write: the verified
    /// installer is downloaded and the user is told how to run it; nothing is elevated.
    Managed {
        /// `"deb"` or `"pkg"`.
        installer: &'static str,
    },
    /// A build inside a cargo `target` directory: never replaced.
    DevBuild,
    /// An installation that needs a different mechanism (macOS bundle).
    Unsupported(&'static str),
}

impl InstallKind {
    /// Stable name used in answers.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Standalone { .. } => "standalone",
            Self::AppImage { .. } => "appimage",
            Self::Prefix { .. } => "prefix",
            Self::Managed { .. } => "managed",
            Self::DevBuild => "dev_build",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

/// The file or directory an update replaces; `None` when the kind is not replaced in place.
pub fn target_of(kind: &InstallKind) -> Option<PathBuf> {
    match kind {
        InstallKind::Standalone { exe } => Some(exe.clone()),
        InstallKind::AppImage { file } => Some(file.clone()),
        InstallKind::Prefix { prefix, .. } => Some(prefix.join("lib/spiderhead/bin")),
        _ => None,
    }
}

/// Whether the current user can create entries in `dir`.
fn writable(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(dir.as_os_str().as_bytes())
            // SAFETY: `path` is a valid NUL-terminated string for the duration of the call.
            .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// The profile id in `<prefix>/share/spiderhead/profile.toml`, when it names a known profile.
fn read_profile(prefix: &Path) -> Option<String> {
    let text = std::fs::read_to_string(prefix.join("share/spiderhead/profile.toml")).ok()?;
    let value = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "profile").then(|| value.trim().trim_matches('"').to_owned())
    })?;
    matches!(value.as_str(), "lite" | "cli" | "desktop" | "all").then_some(value)
}

/// Classify the running binary. `exe` is the canonical path of the current executable and
/// `appimage` the value of `$APPIMAGE`.
pub fn detect(exe: &Path, appimage: Option<&Path>) -> InstallKind {
    let parts: Vec<_> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    if parts
        .windows(2)
        .any(|w| w[0] == "target" && matches!(&*w[1], "debug" | "release"))
    {
        return InstallKind::DevBuild;
    }
    if let Some(file) = appimage.filter(|f| f.is_file()) {
        return InstallKind::AppImage {
            file: file.to_path_buf(),
        };
    }
    if parts
        .windows(3)
        .any(|w| w[0].ends_with(".app") && w[1] == "Contents" && w[2] == "MacOS")
    {
        return InstallKind::Unsupported("macOS application bundle");
    }
    let lib = exe.parent().and_then(Path::parent);
    let layout = exe
        .parent()
        .is_some_and(|bin| bin.ends_with("lib/spiderhead/bin"));
    if let (true, Some(lib)) = (layout, lib) {
        let prefix = lib.parent().and_then(Path::parent);
        if let Some(prefix) = prefix {
            if !writable(lib) {
                let installer = if cfg!(target_os = "macos") {
                    "pkg"
                } else {
                    "deb"
                };
                return InstallKind::Managed { installer };
            }
            return match read_profile(prefix) {
                Some(profile) => InstallKind::Prefix {
                    prefix: prefix.to_path_buf(),
                    profile,
                },
                None => InstallKind::Unsupported("installation without a profile.toml"),
            };
        }
    }
    InstallKind::Standalone {
        exe: exe.to_path_buf(),
    }
}

/// The release asset that carries `version` for this install kind.
pub fn asset_for(kind: &InstallKind, version: &str) -> Option<String> {
    let triple = target_triple()?;
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    match kind {
        InstallKind::Standalone { .. } => Some(format!("spiderhead-{version}-{triple}")),
        InstallKind::Prefix { profile, .. } => {
            Some(format!("spiderhead-{version}-{profile}-{os}-{arch}.tar.gz"))
        }
        InstallKind::AppImage { .. } if os == "linux" => {
            Some(format!("SpiderHead-{version}-linux-{arch}.AppImage"))
        }
        InstallKind::Managed { installer: "deb" } => {
            let deb = if arch == "aarch64" { "arm64" } else { "amd64" };
            Some(format!("spiderhead_{version}_{deb}.deb"))
        }
        InstallKind::Managed { installer: "pkg" } => {
            let pkg = if arch == "aarch64" { "arm64" } else { "x86_64" };
            Some(format!("SpiderHead-{version}-macos-{pkg}.pkg"))
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
        json!({"target": target.to_string_lossy(), "step": step, "dir": target.is_dir()})
            .to_string(),
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

/// Replace the directory `target` with `staged`, which must already sit inside the staging area
/// of `target` (same filesystem). The staged tree is probed, the old directory is kept as `.prev`,
/// the swap is two renames, and the new tree is probed again; a failed probe restores the old one.
/// A crash between the renames is repaired by [`recover`].
pub fn replace_dir(
    staged: &Path,
    target: &Path,
    probe: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Replaced, InstallError> {
    let stage = prepare_stage(target)?;
    let backup = backup_path(target);
    let journal = stage.join("journal.json");
    probe(staged).map_err(InstallError::ProbeFailed)?;
    let _ = std::fs::remove_dir_all(&backup);
    write_journal(&stage, target, "swapped")?;
    if target.exists() {
        if let Err(error) = std::fs::rename(target, &backup) {
            let _ = std::fs::remove_file(&journal);
            return Err(error.into());
        }
    }
    if let Err(error) = std::fs::rename(staged, target) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, target);
        }
        let _ = std::fs::remove_file(&journal);
        return Err(error.into());
    }
    if let Err(reason) = probe(target) {
        if backup.exists() {
            std::fs::remove_dir_all(target)?;
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
    if target.is_dir() {
        let discard = stage_dir(target).join(format!("{}.discard", file_name(target)));
        let _ = std::fs::remove_dir_all(&discard);
        std::fs::rename(target, &discard)?;
        std::fs::rename(&backup, target)?;
        let _ = std::fs::remove_dir_all(&discard);
    } else {
        std::fs::rename(&backup, target)?;
    }
    Ok(())
}

fn remove_any(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir_all(path);
}

/// Finish or undo an update that was interrupted; the answer says what was done.
pub fn recover(target: &Path) -> Option<String> {
    let stage = stage_dir(target);
    let journal = stage.join("journal.json");
    let state: Value = serde_json::from_slice(&std::fs::read(&journal).ok()?).ok()?;
    let _ = std::fs::remove_file(&journal);
    remove_any(&stage.join(format!("{}.new", file_name(target))));
    if state["step"] != "swapped" {
        return Some("removed the leftovers of an interrupted update".into());
    }
    let backup = backup_path(target);
    let is_dir = state["dir"] == true;
    let runs = if is_dir {
        // A directory install is healthy when it exists and its CLI (if it has one) runs.
        let cli = target.join("spiderhead");
        target.is_dir()
            && (!cli.exists()
                || Command::new(cli)
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success()))
    } else {
        Command::new(target)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if runs || !backup.exists() {
        return None;
    }
    if is_dir {
        remove_any(target);
    }
    std::fs::rename(&backup, target)
        .is_ok()
        .then(|| "restored the previous version after an interrupted update".into())
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
        // A concurrently spawned child (other tests run `tar`) can hold the descriptor for the
        // instant between fork and exec, so the release is polled.
        let freed = (0..100).any(|_| {
            lock(tmp.path()).is_ok() || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
        });
        assert!(freed);
    }

    #[test]
    fn install_kind_follows_the_path() {
        let standalone = detect(Path::new("/home/u/.local/bin/spiderhead"), None);
        assert_eq!(
            standalone,
            InstallKind::Standalone {
                exe: "/home/u/.local/bin/spiderhead".into()
            }
        );
        assert_eq!(
            detect(Path::new("/w/repo/target/release/spiderhead"), None),
            InstallKind::DevBuild
        );
        let bundle = detect(
            Path::new("/x/SpiderHead.app/Contents/MacOS/spiderhead"),
            None,
        );
        assert!(matches!(bundle, InstallKind::Unsupported(_)));
    }

    #[test]
    fn appimage_and_prefix_and_managed_are_told_apart() {
        let tmp = tempfile::tempdir().expect("tmp");
        let image = tmp.path().join("SpiderHead.AppImage");
        std::fs::write(&image, b"x").expect("image");
        assert_eq!(
            detect(Path::new("/tmp/.mount_x/usr/bin/spiderhead"), Some(&image)),
            InstallKind::AppImage { file: image }
        );

        // A writable prefix with a profile is a Prefix install; without one it is refused.
        let prefix = tmp.path().join("p");
        let bin = prefix.join("lib/spiderhead/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let exe = bin.join("spiderhead");
        assert!(matches!(detect(&exe, None), InstallKind::Unsupported(_)));
        std::fs::create_dir_all(prefix.join("share/spiderhead")).expect("share");
        std::fs::write(
            prefix.join("share/spiderhead/profile.toml"),
            "profile = \"desktop\"\nversion = \"1\"\n",
        )
        .expect("profile");
        assert_eq!(
            detect(&exe, None),
            InstallKind::Prefix {
                prefix: prefix.clone(),
                profile: "desktop".into()
            }
        );
        assert_eq!(target_of(&detect(&exe, None)), Some(bin.clone()));

        // The same layout the user cannot write belongs to a package manager.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let lib = prefix.join("lib/spiderhead");
            std::fs::set_permissions(&lib, std::fs::Permissions::from_mode(0o555)).expect("chmod");
            let denied = !writable(&lib);
            let kind = detect(&exe, None);
            std::fs::set_permissions(&lib, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            // root ignores permission bits, so assert only where they apply.
            if denied {
                assert!(matches!(kind, InstallKind::Managed { .. }));
            }
        }
    }

    #[test]
    fn asset_names_follow_the_release_layout() {
        let prefix = InstallKind::Prefix {
            prefix: "/p".into(),
            profile: "cli".into(),
        };
        if target_triple().is_some() {
            let name = asset_for(&prefix, "1.2.3").expect("name");
            assert!(name.starts_with("spiderhead-1.2.3-cli-") && name.ends_with(".tar.gz"));
            assert!(
                asset_for(&InstallKind::Managed { installer: "deb" }, "1.2.3")
                    .is_some_and(|n| n.starts_with("spiderhead_1.2.3_") && n.ends_with(".deb"))
            );
            assert!(
                asset_for(&InstallKind::Managed { installer: "pkg" }, "1.2.3").is_some_and(|n| n
                    .starts_with("SpiderHead-1.2.3-macos-")
                    && n.ends_with(".pkg"))
            );
        }
        assert_eq!(asset_for(&InstallKind::DevBuild, "1.2.3"), None);
    }

    #[test]
    fn replace_dir_swaps_keeps_a_backup_and_rolls_back() {
        let tmp = tempfile::tempdir().expect("tmp");
        let target = tmp.path().join("bin");
        std::fs::create_dir(&target).expect("target");
        std::fs::write(target.join("tool"), b"old").expect("old");
        let stage = prepare_stage(&target).expect("stage");
        let staged = |bytes: &[u8]| {
            let dir = stage.join("bin.new");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir(&dir).expect("staged");
            std::fs::write(dir.join("tool"), bytes).expect("new");
            dir
        };

        // A failing pre-swap probe leaves the target alone.
        let early = replace_dir(&staged(b"new"), &target, &|_| Err("no".into()));
        assert!(matches!(early, Err(InstallError::ProbeFailed(_))));
        assert_eq!(std::fs::read(target.join("tool")).unwrap(), b"old");

        // A failing post-swap probe restores the old tree.
        let calls = std::cell::Cell::new(0);
        let late = replace_dir(&staged(b"new"), &target, &|_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err("crashes".into())
            } else {
                Ok(())
            }
        });
        assert!(matches!(late, Err(InstallError::RolledBack(_))));
        assert_eq!(std::fs::read(target.join("tool")).unwrap(), b"old");

        // Success keeps the previous generation, and rollback brings it back.
        let done = replace_dir(&staged(b"new"), &target, &|_| Ok(())).expect("replace");
        assert_eq!(std::fs::read(target.join("tool")).unwrap(), b"new");
        assert_eq!(std::fs::read(done.backup.join("tool")).unwrap(), b"old");
        rollback(&target).expect("rollback");
        assert_eq!(std::fs::read(target.join("tool")).unwrap(), b"old");
        assert!(matches!(rollback(&target), Err(InstallError::NoBackup)));
    }

    #[test]
    fn recover_puts_a_directory_back_after_a_crash_between_the_renames() {
        let tmp = tempfile::tempdir().expect("tmp");
        let target = tmp.path().join("bin");
        std::fs::create_dir(&target).expect("target");
        let stage = prepare_stage(&target).expect("stage");
        write_journal(&stage, &target, "swapped").expect("journal");
        // The crash happened after the old tree was moved aside and before the new one arrived.
        std::fs::rename(&target, backup_path(&target)).expect("aside");
        std::fs::write(backup_path(&target).join("tool"), b"old").expect("old");
        assert!(recover(&target).is_some());
        assert_eq!(std::fs::read(target.join("tool")).unwrap(), b"old");
        assert!(recover(&target).is_none());
    }
}

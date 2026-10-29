//! Comparison guards and byte/hash primitives: cancellation checks, the
//! no-replace directory rename, checkpoint member safety, canonical JSON and
//! file digests — the vocabulary both the write and read engines use.

use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use spiderhead_core::common::canonical_json;
use spiderhead_core::job_control::JobProgressUpdate;

use crate::compare::base::{os_error_text, CompareError};

/// Bucket names of the saved comparison, in report order. `unchanged` rows are
/// exported as evidence for reviews, not counted as differences.
pub(crate) const BUCKETS: [&str; 5] = ["entered", "left", "appeared", "disappeared", "unchanged"];

/// The inclusive upper bound of SQLite integers; checkpoints never exceed it.
pub(crate) const SQLITE_MAX_INTEGER: i64 = i64::MAX;

/// the reference canonical-JSON encoding used for digests, checkpoints and exports.
pub(crate) fn py_json(value: &serde_json::Value) -> String {
    canonical_json(value)
}

/// Refuse work once the caller's cancellation flag is raised.
pub(crate) fn check_cancel(cancel_event: Option<&dyn Fn() -> bool>) -> Result<(), CompareError> {
    if cancel_event.is_some_and(|event| event()) {
        return Err(CompareError("comparison was cancelled".into()));
    }
    Ok(())
}

/// Atomically publish a directory without replacing any existing destination.
///
/// Errors carry the destination path in the reference `OSError` rendering so failure
/// reasons recorded in `job.json` read the same.
#[cfg(target_os = "macos")]
pub(crate) fn rename_directory_noreplace(
    source: &Path,
    destination: &Path,
) -> Result<(), CompareError> {
    use std::ffi::CString;
    let source = CString::new(source.as_os_str().as_encoded_bytes())
        .map_err(|_| CompareError("comparison path contains a NUL byte".into()))?;
    let destination_c = CString::new(destination.as_os_str().as_encoded_bytes())
        .map_err(|_| CompareError("comparison path contains a NUL byte".into()))?;
    let result = unsafe { libc::renamex_np(source.as_ptr(), destination_c.as_ptr(), RENAME_EXCL) };
    if result != 0 {
        return Err(CompareError(os_error_text(
            &std::io::Error::last_os_error(),
            destination,
        )));
    }
    Ok(())
}

/// Atomically publish a directory without replacing any existing destination.
#[cfg(target_os = "linux")]
pub(crate) fn rename_directory_noreplace(
    source: &Path,
    destination: &Path,
) -> Result<(), CompareError> {
    use std::ffi::CString;
    let source = CString::new(source.as_os_str().as_encoded_bytes())
        .map_err(|_| CompareError("comparison path contains a NUL byte".into()))?;
    let destination_c = CString::new(destination.as_os_str().as_encoded_bytes())
        .map_err(|_| CompareError("comparison path contains a NUL byte".into()))?;
    const AT_FDCWD: libc::c_int = -100;
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let result = unsafe {
        libc::renameat2(
            AT_FDCWD,
            source.as_ptr(),
            AT_FDCWD,
            destination_c.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(CompareError(os_error_text(
            &std::io::Error::last_os_error(),
            destination,
        )));
    }
    Ok(())
}

/// Atomically publish a directory without replacing any existing destination.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn rename_directory_noreplace(
    source: &Path,
    destination: &Path,
) -> Result<(), CompareError> {
    if destination.exists() || destination.is_symlink() {
        return Err(CompareError(os_error_text(
            &std::io::Error::from_raw_os_error(libc::EEXIST),
            destination,
        )));
    }
    std::fs::rename(source, destination)
        .map_err(|error| CompareError(os_error_text(&error, destination)))
}

#[cfg(target_os = "macos")]
const RENAME_EXCL: libc::c_uint = 0x0000_0004;

/// Refuse to touch a checkpoint member that is missing (unless optional), a
/// symlink, of the wrong type, or — for files — hard-linked beyond `max_links`.
pub(crate) fn safe_member(
    path: &Path,
    directory: bool,
    optional: bool,
    max_links: u64,
) -> Result<(), CompareError> {
    let info = match std::fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if optional {
                return Ok(());
            }
            return Err(CompareError(format!(
                "comparison checkpoint member is missing: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        Err(error) => {
            return Err(CompareError(os_error_text(
                &error,
                &path.join(path.file_name().unwrap_or_default()),
            )));
        }
    };
    let mode = info.mode();
    let is_dir = info.is_dir();
    let type_ok = if directory { is_dir } else { info.is_file() };
    if (mode & (libc::S_IFMT as u32)) == (libc::S_IFLNK as u32) || !type_ok {
        return Err(CompareError(format!(
            "comparison checkpoint member is unsafe: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }
    if !directory && info.nlink() > max_links {
        return Err(CompareError(format!(
            "comparison checkpoint member has unexpected hard links: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }
    Ok(())
}

/// Flush directory entries on POSIX so a completed rename survives a crash.
pub(crate) fn fsync_directory(path: &Path) -> Result<(), CompareError> {
    let file = File::open(path).map_err(|error| CompareError(os_error_text(&error, path)))?;
    file.sync_all()
        .map_err(|error| CompareError(os_error_text(&error, path)))
}

/// SHA-256 hex digest and byte count of a file, streamed in 1 MiB blocks.
pub(crate) fn file_sha256_with_size(path: &Path) -> Result<(String, u64), CompareError> {
    let mut file = File::open(path).map_err(|error| CompareError(os_error_text(&error, path)))?;
    let mut digest = sha2::Sha256::new();
    let mut size: u64 = 0;
    let mut block = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut block)
            .map_err(|error| CompareError(os_error_text(&error, path)))?;
        if read == 0 {
            break;
        }
        digest.update(&block[..read]);
        size += read as u64;
    }
    use sha2::Digest as _;
    Ok((hex(&digest.finalize()), size))
}

/// Lowercase hex encoding of a digest.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0xf) as u32, 16).unwrap_or('0'));
    }
    out
}

/// An open, non-aliased lock file: refuses symlinks and keeps the descriptor so
/// the advisory lock lives exactly as long as the holder.
pub(crate) struct LockFile {
    file: File,
}

impl LockFile {
    /// Open (creating unless refused) and flock the lock file exclusively.
    pub fn open(path: &Path, create: bool) -> Result<Self, CompareError> {
        if path.is_symlink() {
            return Err(CompareError(
                "writer lock path must not be a symlink".into(),
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        if create {
            options.create(true);
        }
        options.mode(0o600);
        let nofollow = libc::O_NOFOLLOW;
        options.custom_flags(nofollow);
        let file = options
            .open(path)
            .map_err(|error| CompareError(os_error_text(&error, path)))?;
        let opened = file
            .metadata()
            .map_err(|error| CompareError(os_error_text(&error, path)))?;
        let named = std::fs::symlink_metadata(path)
            .map_err(|error| CompareError(os_error_text(&error, path)))?;
        if !opened.is_file()
            || opened.nlink() != 1
            || opened.dev() != named.dev()
            || opened.ino() != named.ino()
        {
            return Err(CompareError(
                "writer lock must be a regular unaliased file".into(),
            ));
        }
        Ok(Self { file })
    }

    /// Try a nonblocking exclusive lock; contention returns an error.
    pub fn try_lock_exclusive(&self) -> Result<(), CompareError> {
        let result = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(CompareError(os_error_text(
                &std::io::Error::last_os_error(),
                Path::new("<lock>"),
            )));
        }
        Ok(())
    }

    /// Release the advisory lock.
    pub fn unlock(&self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Whether a live comparison writer holds the work directory's exclusive lock
/// right now. An absent or unopenable lock file is not evidence of activity —
/// only a contended one is, matching the job-status probe.
pub fn writer_is_active(work: &Path) -> bool {
    let lock_path = work.join(".writer.lock");
    let Ok(lock) = LockFile::open(&lock_path, false) else {
        return false;
    };
    if lock.try_lock_exclusive().is_err() {
        return true;
    }
    lock.unlock();
    false
}

/// Attach the job identity to one progress update; a broken transport must not
/// corrupt or stop the saved comparison, so construction failures are swallowed.
pub(crate) fn notify(
    progress: Option<&dyn Fn(&JobProgressUpdate)>,
    job_id: &str,
    state: &str,
    stage: &str,
    completed: Option<i64>,
    total: Option<i64>,
    reason: Option<&str>,
) {
    let Some(progress) = progress else {
        return;
    };
    if let Ok(update) = JobProgressUpdate::new(job_id, state, stage, completed, total, reason) {
        progress(&update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_member_rejects_symlink_and_wrong_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("file"), b"x").expect("write");
        std::fs::create_dir(root.join("subdir")).expect("mkdir");
        #[cfg(unix)]
        std::os::unix::fs::symlink("file", root.join("link")).expect("symlink");
        safe_member(&root.join("file"), false, false, 1).expect("file ok");
        safe_member(&root.join("subdir"), true, false, 1).expect("dir ok");
        assert_eq!(
            safe_member(&root.join("link"), false, false, 1)
                .err()
                .map(|e| e.0),
            Some("comparison checkpoint member is unsafe: link".into())
        );
        assert_eq!(
            safe_member(&root.join("missing"), false, false, 1)
                .err()
                .map(|e| e.0),
            Some("comparison checkpoint member is missing: missing".into())
        );
        assert!(safe_member(&root.join("missing"), false, true, 1).is_ok());
    }

    #[test]
    fn safe_member_rejects_extra_hard_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("a"), b"x").expect("write");
        std::fs::hard_link(root.join("a"), root.join("b")).expect("hard link");
        assert_eq!(
            safe_member(&root.join("a"), false, false, 1)
                .err()
                .map(|e| e.0),
            Some("comparison checkpoint member has unexpected hard links: a".into())
        );
        safe_member(&root.join("a"), false, false, 2).expect("two links allowed");
    }
}

//! Advisory file locks and directory fsync.
//!
//! The core crate defers its locking layer, so the project package keeps its own:
//! `File` lock methods (`flock(2)` on POSIX, `LockFileEx` on Windows).
//! `open_lock` refuses symlinks and aliased files.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use spiderhead_core::filesystem::{file_identity, link_identity, NoFollow, PrivateMode};

/// Locking is always available: the standard library maps it to the platform API.
pub fn require_locking() -> io::Result<()> {
    Ok(())
}

/// Nonblocking exclusive lock (`flock LOCK_EX|LOCK_NB`); `WouldBlock` while another
/// owner holds it.
pub fn lock_exclusive(file: &File) -> io::Result<()> {
    file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
        std::fs::TryLockError::Error(e) => e,
    })
}

/// Blocking exclusive lock (`flock LOCK_EX`); waits while another owner holds it.
pub fn lock_exclusive_wait(file: &File) -> io::Result<()> {
    file.lock()
}

/// Shared read lock (`flock LOCK_SH`); waits while a writer holds it.
pub fn lock_shared(file: &File) -> io::Result<()> {
    file.lock_shared()
}

/// Release the lock (`flock LOCK_UN`).
pub fn unlock(file: &File) -> io::Result<()> {
    file.unlock()
}

/// Open an unaliased regular lock file, refusing symlinks and reparse points (`open_lock`).
///
/// `create` adds `O_CREAT` with mode `0o600`.
pub fn open_lock(path: &Path, create: bool) -> io::Result<File> {
    if path.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "writer lock path must not be a symlink",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).no_follow();
    if create {
        options.create(true).private_mode(0o600);
    }
    let file = options.open(path)?;
    check_unaliased(&file, path)?;
    Ok(file)
}

/// Same-file, regular-file and link-count checks; the file is closed on failure.
fn check_unaliased(file: &File, path: &Path) -> io::Result<()> {
    let opened = file.metadata()?;
    let opened_id = file_identity(file)?;
    let named_id = link_identity(path)?;
    let ok = opened.is_file()
        && opened_id.2 == 1
        && (opened_id.0, opened_id.1) == (named_id.0, named_id.1);
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "writer lock must be a regular unaliased file",
        ))
    }
}

/// `fsync_directory`: flush directory entries where the platform exposes directory fsync;
/// a no-op where it does not (Windows opens directories only for metadata).
#[cfg(unix)]
pub fn fsync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// `fsync_directory` where directory fsync is not exposed.
#[cfg(not(unix))]
pub fn fsync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

//! Filesystem helpers shared by local workflows: streamed SHA-256, atomic
//! publication and the platform facts (`stat`-style metadata, free space,
//! creation modes) that file-owning workflows need.

use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Read chunk size for hashing: 1 MiB.
const CHUNK: usize = 1 << 20;

/// SHA-256 hex digest of a regular file, streamed in 1 MiB chunks.
pub fn file_sha256(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex(&digest.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique staging path in `directory` (no two callers share one).
fn staging_path(directory: &Path, prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    directory.join(format!("{prefix}{}-{nanos}-{seq}.tmp", std::process::id()))
}

/// Replace `path` with `data` so readers see the old or the new file, never a mix.
///
/// `durable` fsyncs the staged file and the parent directory. `mode` sets the Unix mode
/// of the staged file before publication. A failed publish removes the staged file and
/// leaves the previous content in place.
pub fn atomic_write_bytes(
    path: &Path,
    data: &[u8],
    prefix: Option<&str>,
    mode: Option<u32>,
    durable: bool,
) -> io::Result<()> {
    atomic_write_with(path, data, prefix, mode, durable, &|from, to| {
        fs::rename(from, to)
    })
}

/// The body of [`atomic_write_bytes`] with the publish step injected, so the failure path
/// is testable: tests replace the publish step to force a failure.
pub(crate) fn atomic_write_with(
    path: &Path,
    data: &[u8],
    prefix: Option<&str>,
    mode: Option<u32>,
    durable: bool,
    publish: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = prefix
        .map(str::to_owned)
        .unwrap_or_else(|| format!(".{name}."));
    let staged = staging_path(directory, &prefix);

    let staged_result = (|| -> io::Result<()> {
        let mut file = File::create(&staged)?;
        file.write_all(data)?;
        file.flush()?;
        if durable {
            file.sync_all()?;
        }
        drop(file);
        set_mode(&staged, mode)?;
        Ok(())
    })();
    if let Err(error) = staged_result {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }

    let published = publish(&staged, path);
    let _ = fs::remove_file(&staged);
    published?;
    if durable {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match mode {
        Some(bits) => fs::set_permissions(path, fs::Permissions::from_mode(bits)),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: Option<u32>) -> io::Result<()> {
    Ok(())
}

/// `chmod(path, bits)` where permission modes exist; a no-op elsewhere, where
/// the object keeps the ACL inherited from its parent directory.
pub fn set_permissions_mode(path: &Path, bits: u32) -> io::Result<()> {
    set_mode(path, Some(bits))
}

/// Fields the code reads out of `stat(2)`: mode bits and nanosecond
/// timestamps.
///
/// Windows has no mode word; ACLs on user-profile directories already restrict
/// objects to their owner, so `mode` reports `0o600` there. The 100 ns Windows
/// file time is folded down to seconds + nanoseconds since the Unix epoch.
pub trait StatCompat {
    /// Permission bits (`st_mode` low bits).
    fn mode(&self) -> u32;
    /// Seconds since the Unix epoch of the last write (`st_mtime`).
    fn mtime(&self) -> i64;
    /// Sub-second part of the last write, in nanoseconds (`st_mtime_nsec`).
    fn mtime_nsec(&self) -> i64;
    /// Sub-second part of the change/creation time (`st_ctime_nsec`; birth time
    /// where `st_ctime` does not exist).
    fn ctime_nsec(&self) -> i64;
    /// File size in bytes (`st_size`, the same as [`Metadata::len`]).
    fn size(&self) -> u64;
}

#[cfg(unix)]
impl StatCompat for Metadata {
    fn mode(&self) -> u32 {
        std::os::unix::fs::MetadataExt::mode(self)
    }
    fn mtime(&self) -> i64 {
        std::os::unix::fs::MetadataExt::mtime(self)
    }
    fn mtime_nsec(&self) -> i64 {
        std::os::unix::fs::MetadataExt::mtime_nsec(self)
    }
    fn ctime_nsec(&self) -> i64 {
        std::os::unix::fs::MetadataExt::ctime_nsec(self)
    }
    fn size(&self) -> u64 {
        self.len()
    }
}

#[cfg(windows)]
impl StatCompat for Metadata {
    fn mode(&self) -> u32 {
        0o600
    }
    fn mtime(&self) -> i64 {
        (self.write_time_nanos() / 1_000_000_000) as i64
    }
    fn mtime_nsec(&self) -> i64 {
        (self.write_time_nanos() % 1_000_000_000) as i64
    }
    fn ctime_nsec(&self) -> i64 {
        (self.creation_time_nanos() % 1_000_000_000) as i64
    }
    fn size(&self) -> u64 {
        self.len()
    }
}

#[cfg(windows)]
trait FileTimeNanos {
    /// Last write time as nanoseconds since the Unix epoch.
    fn write_time_nanos(&self) -> u64;
    /// Creation time as nanoseconds since the Unix epoch.
    fn creation_time_nanos(&self) -> u64;
}

#[cfg(windows)]
impl FileTimeNanos for Metadata {
    fn write_time_nanos(&self) -> u64 {
        windows_ticks_to_nanos(std::os::windows::fs::MetadataExt::last_write_time(self))
    }
    fn creation_time_nanos(&self) -> u64 {
        windows_ticks_to_nanos(std::os::windows::fs::MetadataExt::creation_time(self))
    }
}

/// Fold Windows 100 ns file-time ticks down to nanoseconds since the Unix epoch.
#[cfg(windows)]
fn windows_ticks_to_nanos(ticks: u64) -> u64 {
    /// 100 ns ticks between the Windows epoch (1601) and the Unix epoch.
    const TICKS_TO_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
    ticks
        .saturating_sub(TICKS_TO_UNIX_EPOCH)
        .saturating_mul(100)
}

/// `(st_dev, st_ino, st_nlink)` — the platform's file identity.
///
/// On Unix the triple comes straight from `stat`/`fstat`; on Windows it is the
/// `BY_HANDLE_FILE_INFORMATION` triple (volume serial number, 64-bit file
/// index, link count), the same data `os.stat` reports there.
pub type FileIdentity = (u64, u64, u64);

/// Identity of `file`: `fstat` on Unix, `GetFileInformationByHandle` on
/// Windows.
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    file_identity_impl(file)
}

#[cfg(unix)]
fn file_identity_impl(file: &File) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok((meta.dev(), meta.ino(), meta.nlink()))
}

#[cfg(windows)]
fn file_identity_impl(file: &File) -> io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    handle_identity(file.as_raw_handle())
}

#[cfg(windows)]
fn handle_identity(handle: std::os::windows::io::RawHandle) -> io::Result<FileIdentity> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a writable BY_HANDLE_FILE_INFORMATION out-parameter.
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        index,
        u64::from(info.nNumberOfLinks),
    ))
}

/// Open `path` for an identity query: directories and reparse points need the
/// flags Windows asks for (`FILE_FLAG_BACKUP_SEMANTICS`, optionally
/// `FILE_FLAG_OPEN_REPARSE_POINT`).
#[cfg(windows)]
fn open_identity(path: &Path, reparse_point: bool) -> io::Result<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(if reparse_point {
            BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        } else {
            BACKUP_SEMANTICS
        })
        .open(path)?;
    file_identity_impl(&file)
}

/// Identity of what `path` names (a symlink resolves to its target).
pub fn path_identity(path: &Path) -> io::Result<FileIdentity> {
    path_identity_impl(path)
}

#[cfg(unix)]
fn path_identity_impl(path: &Path) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    Ok((meta.dev(), meta.ino(), meta.nlink()))
}

#[cfg(windows)]
fn path_identity_impl(path: &Path) -> io::Result<FileIdentity> {
    open_identity(path, false)
}

/// Identity of `path` itself: the symlink, not its target.
pub fn link_identity(path: &Path) -> io::Result<FileIdentity> {
    link_identity_impl(path)
}

#[cfg(unix)]
fn link_identity_impl(path: &Path) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(path)?;
    Ok((meta.dev(), meta.ino(), meta.nlink()))
}

#[cfg(windows)]
fn link_identity_impl(path: &Path) -> io::Result<FileIdentity> {
    open_identity(path, true)
}

/// Refuse to follow a symlink at the last path component when opening.
///
/// On Unix this is `O_NOFOLLOW`: the open itself fails on a symlink. Windows
/// has no such flag; `FILE_FLAG_OPEN_REPARSE_POINT` opens the reparse point
/// itself, so the file-type and identity checks callers run afterwards see the
/// link rather than its target and reject it.
pub trait NoFollow {
    /// Chainable no-follow flag for builder-style call sites.
    fn no_follow(&mut self) -> &mut Self;
}

impl NoFollow for OpenOptions {
    fn no_follow(&mut self) -> &mut Self {
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(self, libc::O_NOFOLLOW);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::custom_flags(self, 0x0020_0000);
        self
    }
}

/// Unix permission `mode` applied at creation time.
///
/// Where permission modes exist this forwards to `OpenOptionsExt::mode` /
/// `DirBuilderExt::mode`; elsewhere it is a no-op and the created object
/// inherits the ACL of its parent directory.
pub trait PrivateMode {
    /// Chainable `mode(bits)` for builder-style call sites.
    fn private_mode(&mut self, mode: u32) -> &mut Self;
}

impl PrivateMode for OpenOptions {
    fn private_mode(&mut self, mode: u32) -> &mut Self {
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(self, mode);
        #[cfg(not(unix))]
        let _ = mode;
        self
    }
}

impl PrivateMode for DirBuilder {
    fn private_mode(&mut self, mode: u32) -> &mut Self {
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(self, mode);
        #[cfg(not(unix))]
        let _ = mode;
        self
    }
}

/// Free bytes on the filesystem holding `path`.
pub fn free_space(path: &Path) -> io::Result<u64> {
    free_space_impl(path)
}

#[cfg(unix)]
fn free_space_impl(path: &Path) -> io::Result<u64> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `statvfs` is plain data, zero is a valid initial value, and
    // `c_path` is NUL-terminated for the duration of the call.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(windows)]
fn free_space_impl(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated; `free` is a writable out-parameter.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut free,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-core-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    #[cfg(unix)]
    fn publishes_bytes_with_requested_mode_and_no_leftover_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("atomic-ok");
        let target = dir.join("state.json");
        atomic_write_bytes(&target, b"one", None, None, true).unwrap();
        atomic_write_bytes(&target, b"two", None, Some(0o640), true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(names(&dir), vec!["state.json".to_owned()]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_replace_keeps_previous_content_and_removes_temp() {
        let dir = temp_dir("atomic-fail");
        let target = dir.join("state.json");
        fs::write(&target, b"old").unwrap();
        let refuse = |_: &Path, _: &Path| Err(io::Error::other("replace refused"));
        let error = atomic_write_with(&target, b"new", None, None, true, &refuse).unwrap_err();
        assert!(error.to_string().contains("replace refused"));
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(names(&dir), vec!["state.json".to_owned()]);
        fs::remove_dir_all(&dir).unwrap();
    }

    // Digest of known bytes ("abc"), checked against the standard SHA-256 test vector.
    #[test]
    fn file_sha256_matches_known_digest() {
        let dir = temp_dir("sha");
        let path = dir.join("abc.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}

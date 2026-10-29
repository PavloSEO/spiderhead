//! Split out of `crux`.

use super::*;

/// `root.mkdir(parents=True, mode=0o700, exist_ok=True)` + mkstemp + chmod 600 + replace.
pub(super) fn write_cache_file(dir: &Path, path: &Path, encoded: &[u8]) {
    let fresh = !dir.exists();
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    #[cfg(unix)]
    if fresh {
        let _ = spiderhead_core::filesystem::set_permissions_mode(dir, 0o700);
    }
    #[cfg(not(unix))]
    let _ = fresh;
    if let Ok(staged) = credentials::stage_bytes(dir, ".crux-", encoded, 0o600) {
        if std::fs::rename(&staged, path).is_err() {
            let _ = std::fs::remove_file(&staged);
        }
    }
}

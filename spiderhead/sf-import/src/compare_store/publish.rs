//! Atomic publication of a completed comparison package: a zero-byte
//! reservation names this job's destination, a stale hard-link publication
//! made of exactly the staged inodes may be recovered, and the rename never
//! replaces foreign output.

use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use crate::compare::base::CompareError;

use super::shared::{check_cancel, fsync_directory, rename_directory_noreplace, safe_member};

/// The reservation path a job claims before publishing.
pub(crate) fn reservation_path(destination: &Path, job_id: &str) -> std::path::PathBuf {
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    destination.with_file_name(format!(".{name}.compare-publish-{job_id}"))
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

/// Atomically publish the complete package without replacing foreign output.
pub(crate) fn publish(
    root: &Path,
    destination: &Path,
    job_id: &str,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<(), CompareError> {
    use std::io::Write;
    let reservation = reservation_path(destination, job_id);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&reservation)
    {
        Ok(mut stream) => {
            stream.flush().map_err(|e| CompareError(e.to_string()))?;
            stream.sync_all().map_err(|e| CompareError(e.to_string()))?;
            fsync_directory(
                destination
                    .parent()
                    .ok_or_else(|| CompareError("comparison destination has no parent".into()))?,
            )?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            safe_member(&reservation, false, false, 1)?;
            let size = std::fs::metadata(&reservation)
                .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &reservation)))?
                .len();
            if size != 0 {
                return Err(CompareError(
                    "comparison publish reservation is malformed".into(),
                ));
            }
        }
        Err(error) => {
            return Err(CompareError(crate::compare::base::os_error_text(
                &error,
                &reservation,
            )));
        }
    }

    if destination.exists() {
        safe_member(destination, true, false, 1)?;
        let mut members: Vec<std::path::PathBuf> = std::fs::read_dir(destination)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, destination)))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect();
        members.sort();
        if members.is_empty() {
            return Err(CompareError(format!(
                "comparison output directory already exists and is not owned by this job: {}",
                destination.display()
            )));
        }
        // Recover only a prior hard-link publication whose every visible file is
        // still the exact staged inode. Empty or foreign directories are never
        // adopted.
        for member in &members {
            let staged = root.join(
                member
                    .file_name()
                    .map(|name| name.to_os_string())
                    .unwrap_or_default(),
            );
            if member.is_symlink()
                || !member.is_file()
                || !staged.is_file()
                || !same_file(member, &staged)
            {
                let name = member
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                return Err(CompareError(format!(
                    "comparison publish destination contains an unowned member: {name}"
                )));
            }
        }
        for member in &members {
            std::fs::remove_file(member)
                .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, member)))?;
        }
        std::fs::remove_dir(destination)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, destination)))?;
    }
    check_cancel(cancel_event)?;
    fn parent(destination: &Path) -> Result<&Path, CompareError> {
        destination
            .parent()
            .ok_or_else(|| CompareError("comparison destination has no parent".into()))
    }
    let outcome = rename_directory_noreplace(root, destination)
        .and_then(|()| fsync_directory(parent(destination)?));
    if let Err(error) = outcome {
        if let Ok(info) = std::fs::symlink_metadata(&reservation) {
            if !info.is_symlink() && info.is_file() && info.len() == 0 {
                let _ = std::fs::remove_file(&reservation);
            }
        }
        return Err(error);
    }
    if let Err(error) = std::fs::remove_file(&reservation) {
        return Err(CompareError(crate::compare::base::os_error_text(
            &error,
            &reservation,
        )));
    }
    fsync_directory(parent(destination)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_empty_existing_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("staged");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("entered.ndjson"), b"{}\n").expect("write");
        let destination = dir.path().join("out");
        std::fs::create_dir_all(&destination).expect("mkdir");
        let error = publish(&root, &destination, "job1", None)
            .err()
            .map(|e| e.0);
        assert!(error
            .expect("error")
            .starts_with("comparison output directory already exists"));
    }

    #[test]
    fn recovers_owned_hard_link_publication() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("staged");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("entered.ndjson"), b"{}\n").expect("write");
        let destination = dir.path().join("out");
        std::fs::create_dir_all(&destination).expect("mkdir");
        std::fs::hard_link(
            root.join("entered.ndjson"),
            destination.join("entered.ndjson"),
        )
        .expect("link");
        publish(&root, &destination, "job1", None).expect("publish");
        assert!(destination.join("entered.ndjson").is_file());
        assert!(!root.exists(), "staged tree is renamed away");
        assert!(
            !dir.path().join(".out.compare-publish-job1").exists(),
            "reservation is removed"
        );
    }
}

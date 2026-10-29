//! Locating a project from the current directory, the way git finds `.git`.

use std::path::{Path, PathBuf};

/// Name of the project folder inside a site's working tree.
pub const PROJECT_DIR: &str = "seo";
/// Manifest file name inside the project folder.
pub const MANIFEST: &str = "project.json";

/// The nearest project folder for `start`: `start` itself when it holds `project.json`,
/// otherwise the first `seo/` (with a `project.json`) found walking up the parents.
pub fn discover(start: &Path) -> Option<PathBuf> {
    if start.join(MANIFEST).is_file() {
        return Some(start.to_path_buf());
    }
    start
        .ancestors()
        .map(|dir| dir.join(PROJECT_DIR))
        .find(|dir| dir.join(MANIFEST).is_file())
}

/// [`discover`] from the process working directory.
pub fn discover_here() -> Option<PathBuf> {
    discover(&std::env::current_dir().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &Path) {
        std::fs::create_dir_all(root.join("seo")).unwrap();
        std::fs::write(root.join("seo/project.json"), "{}").unwrap();
    }

    #[test]
    fn walks_up_to_the_nearest_seo_folder() {
        let dir = tempdir("walk");
        project(&dir);
        let deep = dir.join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(discover(&deep), Some(dir.join("seo")));
    }

    #[test]
    fn project_json_at_the_start_directory_counts() {
        let dir = tempdir("here");
        std::fs::write(dir.join("project.json"), "{}").unwrap();
        assert_eq!(discover(&dir), Some(dir.clone()));
    }

    #[test]
    fn nothing_is_found_outside_a_project() {
        let dir = tempdir("none");
        assert_eq!(discover(&dir), None);
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sh-root-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

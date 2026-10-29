//! Wiring the bundled `spiderhead` command into the user's terminal and AI clients.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The `spiderhead` command shipped next to the running executable.
pub fn bundled_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "spiderhead.exe"
    } else {
        "spiderhead"
    };
    let candidate = exe.parent()?.join(name);
    candidate.is_file().then_some(candidate)
}

/// Directories to try, best first: those already on `PATH` that the user owns, then
/// `~/.local/bin`.
fn link_dirs(home: &Path, path_var: &str) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path_var)
        .filter(|dir| dir.starts_with(home))
        .collect();
    let fallback = home.join(".local/bin");
    if !dirs.contains(&fallback) {
        dirs.push(fallback);
    }
    dirs
}

/// Symlink `target` as `spiderhead` into the first writable candidate directory.
///
/// Returns the link path and whether its directory is on `PATH`. An existing link to the
/// same target is reported as success; a different file is never replaced.
pub fn install_link(target: &Path, home: &Path, path_var: &str) -> Result<(PathBuf, bool), String> {
    let on_path = |dir: &Path| std::env::split_paths(path_var).any(|p| p == dir);
    let mut last = String::from("no writable directory found");
    for dir in link_dirs(home, path_var) {
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let link = dir.join("spiderhead");
        match std::fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if std::fs::read_link(&link).ok().as_deref() == Some(target) {
                    return Ok((link, on_path(&dir)));
                }
                if std::fs::remove_file(&link).is_err() {
                    continue;
                }
            }
            Ok(_) => {
                last = format!("{} exists and is not a link", link.display());
                continue;
            }
            Err(_) => {}
        }
        #[cfg(unix)]
        match std::os::unix::fs::symlink(target, &link) {
            Ok(()) => return Ok((link, on_path(&dir))),
            Err(error) => last = error.to_string(),
        }
        #[cfg(not(unix))]
        {
            last = "terminal install is not supported on this system".into();
        }
    }
    Err(last)
}

/// `app.install_cli`: link the bundled command into the terminal's `PATH`.
pub fn install_cli() -> Result<Value, String> {
    let target = bundled_cli().ok_or("the spiderhead command is not bundled with this build")?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let path_var = std::env::var("PATH").unwrap_or_default();
    let (link, on_path) = install_link(&target, &home, &path_var)?;
    let message = if on_path {
        format!("Команда spiderhead установлена: {}", link.display())
    } else {
        format!(
            "Команда установлена: {}. Добавьте {} в PATH",
            link.display(),
            link.parent().unwrap_or(Path::new("")).display()
        )
    };
    Ok(json!({"ok": true, "link": link, "target": target, "on_path": on_path, "message": message}))
}

/// The MCP client configuration for `spiderhead mcp`, as JSON text.
pub fn mcp_snippet(command: &str) -> String {
    let doc = json!({"mcpServers": {"spiderhead": {"command": command, "args": ["mcp"]}}});
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// `app.connect_claude`: the snippet to paste into the client's MCP settings. Nothing is
/// written to any client's files.
pub fn connect_claude() -> Value {
    let command = bundled_cli()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "spiderhead".into());
    let snippet = mcp_snippet(&command);
    json!({
        "snippet": snippet,
        "command": command,
        "clipboard": snippet,
        "message": "Конфигурация MCP скопирована: вставьте её в настройки клиента",
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sh-integrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn link_goes_to_a_path_dir_under_home_and_is_idempotent() {
        let home = scratch("home");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let target = home.join("spiderhead-real");
        std::fs::write(&target, "x").unwrap();
        let path_var = format!("/usr/bin:{}", bin.display());
        let (link, on_path) = install_link(&target, &home, &path_var).unwrap();
        assert_eq!(link, bin.join("spiderhead"));
        assert!(on_path);
        assert!(install_link(&target, &home, &path_var).is_ok());
    }

    #[test]
    fn falls_back_to_local_bin_and_reports_it_is_off_path() {
        let home = scratch("fallback");
        let target = home.join("real");
        std::fs::write(&target, "x").unwrap();
        let (link, on_path) = install_link(&target, &home, "/usr/bin").unwrap();
        assert_eq!(link, home.join(".local/bin/spiderhead"));
        assert!(!on_path);
    }

    #[test]
    fn a_regular_file_is_never_replaced() {
        let home = scratch("file");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("spiderhead"), "mine").unwrap();
        let target = home.join("real");
        std::fs::write(&target, "x").unwrap();
        assert!(install_link(&target, &home, "/usr/bin").is_err());
        assert_eq!(
            std::fs::read_to_string(bin.join("spiderhead")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn the_snippet_runs_the_mcp_subcommand() {
        let doc: Value = serde_json::from_str(&mcp_snippet("/x/spiderhead")).unwrap();
        assert_eq!(doc["mcpServers"]["spiderhead"]["args"], json!(["mcp"]));
        assert_eq!(
            doc["mcpServers"]["spiderhead"]["command"],
            json!("/x/spiderhead")
        );
    }
}

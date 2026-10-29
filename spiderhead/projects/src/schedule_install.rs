//! OS trigger text for `schedule run-due`: launchd plist, cron line or Task
//! Scheduler command text.
//!
//! Nothing here runs on import or during `configure`. Each function writes or returns
//! text only when an operator calls it explicitly. The launchd plist is written to a
//! path the caller gives (default: the user's LaunchAgents folder) and is not loaded;
//! loading it is left to the operator with `launchctl bootstrap`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{value, ProjectsResult};

/// Supported trigger kinds.
pub const KINDS: &[&str] = &["launchd", "cron", "windows"];
/// `INTERVAL_SECONDS`.
pub const INTERVAL_SECONDS: i64 = 3600;

/// Absolute path: join relative paths with cwd, then normalize textually.
fn abspath(directory: &str) -> String {
    let path = Path::new(directory);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    joined
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .to_string()
}

fn label(directory: &str) -> String {
    let digest = Sha256::digest(abspath(directory).as_bytes());
    format!(
        "tech.seohead.schedule.{}",
        digest[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn command(directory: &str, interpreter: &str) -> Vec<String> {
    [
        interpreter.to_owned(),
        "-m".into(),
        "spiderhead.cli".into(),
        "schedule-run-due".into(),
        "--directory".into(),
        abspath(directory),
    ]
    .to_vec()
}

fn default_launchd_path(directory: &str) -> PathBuf {
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", label(directory)))
}

fn default_interpreter() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "spiderhead".into())
}

fn uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// XML plist serialization of the fixed schedule dict, with sorted keys.
fn plist_xml(argv: &[String], directory: &str) -> Vec<u8> {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    out.push_str("<plist version=\"1.0\">\n<dict>\n");
    out.push_str(&format!(
        "\t<key>Label</key>\n\t<string>{}</string>\n",
        esc(&label(directory))
    ));
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for item in argv {
        out.push_str(&format!("\t\t<string>{}</string>\n", esc(item)));
    }
    out.push_str("\t</array>\n");
    out.push_str("\t<key>RunAtLoad</key>\n\t<false/>\n");
    out.push_str(&format!(
        "\t<key>StartInterval</key>\n\t<integer>{INTERVAL_SECONDS}</integer>\n"
    ));
    out.push_str("</dict>\n</plist>\n");
    out.into_bytes()
}

/// Write or return the trigger text for one kind.
pub fn install(
    directory: &str,
    kind: &str,
    dest: Option<&Path>,
    python: Option<&str>,
) -> ProjectsResult<Value> {
    if !KINDS.contains(&kind) {
        return Err(value("kind must be launchd, cron or windows"));
    }
    let interpreter = python
        .map(str::to_owned)
        .unwrap_or_else(default_interpreter);
    let argv = command(directory, &interpreter);
    if kind == "launchd" {
        let path = dest
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_launchd_path(directory));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, plist_xml(&argv, directory))?;
        return Ok(json!({
            "ok": true,
            "kind": kind,
            "written": path.to_string_lossy(),
            "loaded": false,
            "next_step": format!("launchctl bootstrap gui/{} {}", uid(), path.to_string_lossy()),
        }));
    }
    if kind == "cron" {
        let line = format!(
            "0 * * * * {}",
            argv.iter()
                .map(|p| shell_quote(p))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut written = Value::Null;
        if let Some(dest) = dest {
            fs::write(dest, format!("{line}\n"))?;
            written = json!(dest.to_string_lossy());
        }
        return Ok(json!({
            "ok": true,
            "kind": kind,
            "line": line,
            "written": written,
            "installed": false,
            "next_step": "add the line with crontab -e",
        }));
    }
    let command_text = format!(
        "schtasks /Create /SC HOURLY /TN {} /TR {} /F",
        quote_windows(&label(directory)),
        quote_windows(
            &argv
                .iter()
                .map(|p| quote_windows(p))
                .collect::<Vec<_>>()
                .join(" ")
        )
    );
    let mut written = Value::Null;
    if let Some(dest) = dest {
        fs::write(dest, format!("{command_text}\r\n"))?;
        written = json!(dest.to_string_lossy());
    }
    Ok(json!({
        "ok": true,
        "kind": kind,
        "command": command_text,
        "written": written,
        "installed": false,
        "next_step": "run the command in an elevated or user Command Prompt",
    }))
}

/// Remove the written trigger file for one kind and return the hint.
pub fn uninstall(directory: &str, kind: &str, dest: Option<&Path>) -> ProjectsResult<Value> {
    if !KINDS.contains(&kind) {
        return Err(value("kind must be launchd, cron or windows"));
    }
    if kind == "launchd" {
        let path = dest
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_launchd_path(directory));
        let removed = path.exists();
        if removed {
            fs::remove_file(&path)?;
        }
        return Ok(json!({
            "ok": true,
            "kind": kind,
            "removed": if removed { json!(path.to_string_lossy()) } else { Value::Null },
            "next_step": format!("launchctl bootout gui/{} {} if it was loaded", uid(), path.to_string_lossy()),
        }));
    }
    let mut removed_file = Value::Null;
    if let Some(dest) = dest {
        if dest.exists() {
            fs::remove_file(dest)?;
            removed_file = json!(dest.to_string_lossy());
        }
    }
    let hint = if kind == "cron" {
        "delete the line from crontab -e".to_owned()
    } else {
        format!(
            "schtasks /Delete /TN {} /F",
            quote_windows(&label(directory))
        )
    };
    Ok(json!({
        "ok": true,
        "kind": kind,
        "removed": removed_file,
        "next_step": hint,
    }))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_alphanumeric() || "/._-=".contains(ch))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn quote_windows(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

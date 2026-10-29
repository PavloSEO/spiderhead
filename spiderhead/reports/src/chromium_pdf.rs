//! Print a local HTML file to PDF with an installed headless Chromium
//! (Chrome, Edge, Chromium).
//!
//! No browser is bundled or downloaded. Discovery order: `SEOHEAD_CHROME` env var,
//! standard install locations, then `PATH`. A missing browser is a `skipped` result,
//! never an exception and never a silent success.
//!
//! The browser runs with its sandbox enabled, a throwaway profile directory,
//! extensions and first-run UI disabled, and a timeout. The result is accepted only
//! after the file exists, starts with `%PDF` and its size has stopped changing;
//! some builds return before the write completes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use spiderhead_core::envvar::read_env;
use spiderhead_core::host::SystemHost;

pub const ENV_VAR: &str = "SEOHEAD_CHROME";
pub const DEFAULT_TIMEOUT: f64 = 120.0;
const STABLE_CHECKS: u32 = 3;
const POLL: Duration = Duration::from_millis(250);

const PATH_NAMES: [&str; 8] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "microsoft-edge-stable",
    "msedge",
    "chrome",
];

fn standard_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for var in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(var) {
                let root = PathBuf::from(root);
                out.push(root.join("Google/Chrome/Application/chrome.exe"));
                out.push(root.join("Microsoft/Edge/Application/msedge.exe"));
                out.push(root.join("Chromium/Application/chrome.exe"));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        out.push(PathBuf::from(
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ));
        out.push(PathBuf::from(
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for p in [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/microsoft-edge",
            "/snap/bin/chromium",
        ] {
            out.push(PathBuf::from(p));
        }
    }
    out
}

/// Search `PATH` for an executable file.
fn which(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    #[cfg(target_os = "windows")]
    let exts = vec![".exe", ".bat", ".cmd", ""];
    #[cfg(not(target_os = "windows"))]
    let exts = vec![""];
    for dir in std::env::split_paths(&path_var) {
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Locate a Chromium-family executable.
/// Returns `{"path": ...}` or `{"path": null, "reason": ...}`.
pub fn find_browser() -> Value {
    let host = SystemHost;
    if let Some(explicit) = read_env(&host, ENV_VAR).filter(|s| !s.is_empty()) {
        let candidate = PathBuf::from(&explicit);
        if candidate.is_file() {
            return json!({"path": candidate.to_string_lossy(), "source": ENV_VAR});
        }
        return json!({
            "path": Value::Null,
            "reason": format!("{ENV_VAR} points to a missing file: {explicit}"),
        });
    }
    for candidate in standard_locations() {
        if candidate.is_file() {
            return json!({"path": candidate.to_string_lossy(), "source": "standard location"});
        }
    }
    for name in PATH_NAMES {
        if let Some(found) = which(name) {
            return json!({"path": found, "source": "PATH"});
        }
    }
    json!({
        "path": Value::Null,
        "reason": format!(
            "no Chrome, Edge or Chromium executable found; install one or set {ENV_VAR} to its path"
        ),
    })
}

/// The exact argv the browser is launched with.
pub fn browser_command(
    browser: &str,
    html_path: &Path,
    pdf_path: &Path,
    profile_dir: &Path,
) -> Vec<String> {
    vec![
        browser.to_owned(),
        "--headless=new".to_owned(),
        "--disable-gpu".to_owned(),
        "--no-pdf-header-footer".to_owned(),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-extensions".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-sync".to_owned(),
        format!("--user-data-dir={}", profile_dir.display()),
        format!("--print-to-pdf={}", pdf_path.display()),
        html_uri(html_path),
    ]
}

/// `file://` URI for a local path.
fn html_uri(path: &Path) -> String {
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut s = absolute.to_string_lossy().replace('\\', "/");
    if s.starts_with("//") {
        s = format!("file:{}", s);
    } else if s.starts_with('/') {
        s = format!("file://{}", s);
    } else {
        s = format!("file:///{}", s);
    }
    s
}

/// Wait until the PDF exists, has a PDF header and a stable size.
fn wait_for_pdf(path: &Path, deadline: Instant, process: &mut Child) -> Option<String> {
    let mut last_size: i64 = -1;
    let mut stable = 0u32;
    while Instant::now() < deadline {
        if path.is_file() {
            let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as i64;
            if size > 0 && size == last_size {
                stable += 1;
                if stable >= STABLE_CHECKS {
                    let mut buf = [0u8; 5];
                    use std::io::Read;
                    let ok = fs::File::open(path)
                        .and_then(|mut f| f.read_exact(&mut buf))
                        .is_ok();
                    if !ok || &buf != b"%PDF-" {
                        return Some("the browser wrote a file that is not a PDF".to_owned());
                    }
                    return None;
                }
            } else {
                stable = 0;
            }
            last_size = size;
        } else if matches!(process.try_wait(), Ok(Some(status)) if status.code() != Some(0)) {
            // Only a non-zero exit fails early; a clean exit without a file keeps polling.
            return Some("the browser exited without writing a PDF".to_owned());
        }
        std::thread::sleep(POLL);
    }
    Some("the browser did not finish writing the PDF before the timeout".to_owned())
}

fn stop(process: &mut Child) {
    if process.try_wait().ok().flatten().is_none() {
        let _ = process.kill();
        let _ = process.wait();
    }
}

/// Render `html_path` to `pdf_path`; returns status ok/skipped/failed.
pub fn print_to_pdf(
    html_path: &Path,
    pdf_path: &Path,
    browser: Option<&str>,
    timeout: f64,
) -> Value {
    let browser = match browser {
        Some(b) => b.to_owned(),
        None => {
            let found = find_browser();
            match found.get("path").and_then(Value::as_str) {
                Some(p) => p.to_owned(),
                None => {
                    return json!({
                        "status": "skipped",
                        "reason": found.get("reason").cloned().unwrap_or(Value::Null),
                    })
                }
            }
        }
    };
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    // The browser writes beside the destination under a temporary name; only a
    // verified file is moved into place.
    let partial = pdf_path.with_file_name(format!(
        ".{}.partial.pdf",
        pdf_path.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let _ = fs::remove_file(&partial);
    let profile = std::env::temp_dir().join(format!("spiderhead-chromium-{}", std::process::id()));
    let _ = fs::remove_dir_all(&profile);
    let _ = fs::create_dir_all(&profile);

    let command = browser_command(&browser, html_path, &partial, &profile);
    let mut process = match Command::new(&command[0])
        .args(&command[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(p) => p,
        Err(e) => {
            let _ = fs::remove_dir_all(&profile);
            return json!({
                "status": "failed",
                "reason": format!("the browser could not start: {e}"),
            });
        }
    };
    let error = wait_for_pdf(&partial, deadline, &mut process);
    stop(&mut process);
    let _ = fs::remove_dir_all(&profile);
    if let Some(error) = error {
        let code = process
            .try_wait()
            .ok()
            .flatten()
            .and_then(|s| s.code())
            .map(|c| c.to_string())
            .unwrap_or_else(|| "None".to_owned());
        let _ = fs::remove_file(&partial);
        return json!({
            "status": "failed",
            "reason": format!("{error} (exit code {code})"),
        });
    }
    if fs::rename(&partial, pdf_path).is_err() {
        // Cross-device fallback: copy then delete.
        if fs::copy(&partial, pdf_path).is_ok() {
            let _ = fs::remove_file(&partial);
        }
    }
    json!({
        "status": "ok",
        "path": pdf_path.to_string_lossy(),
        "browser": browser,
    })
}

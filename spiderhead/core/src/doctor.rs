//! Read-only local environment diagnostics.
//!
//! Reports prerequisites without making requests or writing files. Directory
//! write access is an OS permission check only, and the MCP HTTP bind probe holds
//! a listening socket for no longer than the probe itself, so neither "available"
//! guarantees the resource at a later start.

use std::net::TcpListener;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::host::Host;

/// Format identifier of the diagnostics document.
pub const DOCTOR_FORMAT: &str = "spiderhead.doctor.v1";
/// Documented default bind of the MCP HTTP transport.
pub const MCP_HTTP_HOST: &str = "127.0.0.1";
/// Documented default port of the MCP HTTP transport.
pub const MCP_HTTP_PORT: u16 = 8321;

/// Executable names of the licensed crawler across platforms.
const SF_EXE_NAMES: [&str; 5] = [
    "ScreamingFrogSEOSpiderCli.exe",
    "ScreamingFrogSEOSpiderCli",
    "ScreamingFrogSEOSpiderLauncher",
    "screamingfrogseospider",
    "screaming-frog-seo-spider",
];

/// Fixed install locations probed after PATH.
const SF_INSTALL_PATHS: [&str; 6] = [
    "/Applications/Screaming Frog SEO Spider.app/Contents/MacOS/ScreamingFrogSEOSpiderCli",
    "/Applications/Screaming Frog SEO Spider.app/Contents/MacOS/ScreamingFrogSEOSpiderLauncher",
    "/usr/bin/screamingfrogseospider",
    "/usr/local/bin/screamingfrogseospider",
    "/opt/screamingfrogseospider/screamingfrogseospider",
    "/snap/bin/screaming-frog-seo-spider",
];

/// Browsers rendered pages can use, in preference order.
const BROWSERS: [&str; 5] = [
    "chromium",
    "chromium-browser",
    "google-chrome",
    "chrome",
    "msedge",
];

/// Options of one diagnostics run; `None` leaves the matching probe unasked.
#[derive(Debug, Default, Clone)]
pub struct DoctorOptions {
    /// Projects directory whose access flags are inspected.
    pub projects_directory: Option<String>,
    /// Crawler configuration path (default `config.json`).
    pub config_path: Option<String>,
    /// Explicit crawler CLI path.
    pub sf_cli: Option<String>,
    /// MCP HTTP port to probe (default [`MCP_HTTP_PORT`]).
    pub mcp_http_port: Option<u16>,
}

/// The first executable named `name` on `PATH`.
fn which(host: &dyn Host, name: &str) -> Option<PathBuf> {
    let paths = host.var("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// `(sf_cli.path, sf_cli.search_paths)` of the configuration file, when readable.
fn configured_cli_paths(path: &str) -> Result<(Option<String>, Vec<String>), ()> {
    let file = Path::new(path);
    if !file.is_file() {
        // A missing default `config.json` means defaults; a missing named file is an error.
        return if file.file_name().and_then(|n| n.to_str()) == Some("config.json") {
            Ok((None, Vec::new()))
        } else {
            Err(())
        };
    }
    let text = std::fs::read_to_string(file).map_err(|_| ())?;
    let document: Value = serde_json::from_str(&text).map_err(|_| ())?;
    let cli = document.get("sf_cli");
    let path = cli
        .and_then(|c| c.get("path"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let search = cli
        .and_then(|c| c.get("search_paths"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok((path, search))
}

/// Locate the crawler CLI anywhere it could reasonably be, most specific first.
fn find_sf_cli(
    host: &dyn Host,
    configured: (Option<String>, Vec<String>),
    explicit: Option<&str>,
) -> Option<PathBuf> {
    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(explicit.map(str::to_owned));
    candidates.extend(host.var("SF_CLI"));
    candidates.extend(host.var("SCREAMINGFROG_CLI"));
    candidates.extend(configured.0);
    candidates.extend(configured.1);
    for name in SF_EXE_NAMES {
        candidates.extend(which(host, name).map(|p| p.to_string_lossy().into_owned()));
    }
    match host.var("SPIDERHEAD_DOCTOR_APP_DIRS") {
        // Test/host override: probe only these directories instead of the fixed locations.
        Some(dirs) => {
            for dir in std::env::split_paths(&dirs).filter(|d| !d.as_os_str().is_empty()) {
                for name in SF_EXE_NAMES {
                    candidates.push(dir.join(name).to_string_lossy().into_owned());
                }
            }
        }
        None => candidates.extend(SF_INSTALL_PATHS.iter().map(|p| (*p).to_owned())),
    }
    candidates
        .into_iter()
        .filter(|candidate| !candidate.is_empty())
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

/// Whether the MCP HTTP transport could take its port right now.
fn mcp_http_bind_state(host: &str, port: u16) -> Value {
    match TcpListener::bind((host, port)) {
        Ok(_) => json!({"state": "available", "host": host, "port": port, "reason": null}),
        Err(error) => {
            let code = error.raw_os_error();
            let text = error.to_string();
            let text = match code {
                Some(code) => format!(
                    "[Errno {code}] {}",
                    text.trim_end_matches(&format!(" (os error {code})"))
                ),
                None => text,
            };
            let busy = error.kind() == std::io::ErrorKind::AddrInUse;
            json!({
                "state": if busy { "busy" } else { "unavailable" },
                "host": host,
                "port": port,
                "reason": text,
            })
        }
    }
}

/// Access flags of the selected projects directory.
fn project_directory(path: Option<&str>) -> Value {
    let Some(path) = path else {
        return json!({
            "state": "not_checked",
            "readable": null,
            "writable": null,
            "reason": "pass a projects directory to inspect its access flags",
        });
    };
    if path.is_empty() || path.chars().count() > 4096 {
        return json!({
            "state": "unavailable",
            "readable": null,
            "writable": null,
            "reason": "the selected projects directory path is invalid",
        });
    }
    let root = expand_user(path);
    let Ok(meta) = std::fs::metadata(&root).and_then(|meta| {
        if meta.is_dir() {
            Ok(meta)
        } else {
            Err(std::io::Error::other("not a directory"))
        }
    }) else {
        return json!({
            "state": "unavailable",
            "readable": null,
            "writable": null,
            "reason": "the selected projects directory is missing or unreadable",
        });
    };
    let readable = std::fs::read_dir(&root).is_ok();
    let writable = readable && !meta.permissions().readonly();
    json!({
        "state": if readable { "available" } else { "unavailable" },
        "readable": readable,
        "writable": writable,
        "reason": if readable && writable { Value::Null } else { json!("directory access flags are incomplete") },
    })
}

/// `~` and `~/…` expanded against `HOME`.
fn expand_user(path: &str) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

/// Report local prerequisites without making requests or writing files.
pub fn doctor(host: &dyn Host, options: &DoctorOptions) -> Value {
    let config_path = options.config_path.as_deref().unwrap_or("config.json");
    let (configured, mut config_reason) = match configured_cli_paths(config_path) {
        Ok(found) => (found, None),
        Err(()) => (
            (None, Vec::new()),
            Some("SF configuration could not be read"),
        ),
    };
    let sf_path = find_sf_cli(host, configured, options.sf_cli.as_deref());
    if sf_path.is_some() {
        config_reason = None;
    }
    let browser = BROWSERS.iter().find_map(|name| which(host, name));
    let file_name = |path: &Path| path.file_name().map(|n| n.to_string_lossy().into_owned());
    json!({
        "format": DOCTOR_FORMAT,
        "python": {
            "state": "not_applicable",
            "version": null,
            "reason": "this build is native and needs no Python runtime",
        },
        "playwright": {
            "state": "not_applicable",
            "version": null,
            "reason": "this build renders through a Chromium DevTools connection",
        },
        "chromium": {
            "state": if browser.is_some() { "available" } else { "unavailable" },
            "executable": browser.as_deref().and_then(file_name),
            "reason": if browser.is_some() {
                Value::Null
            } else {
                json!("no supported Chromium executable is on PATH; rendered pages need it — install one (e.g. `playwright install chromium`, or a Chrome/Chromium/Edge binary on PATH)")
            },
        },
        "screaming_frog": {
            "state": if sf_path.is_some() { "available" } else { "unavailable" },
            "executable": sf_path.as_deref().and_then(file_name),
            "license": {
                "valid": null,
                "checked": null,
                "until": null,
                "reason": "license status is not established by executable discovery",
            },
            "reason": if sf_path.is_some() {
                Value::Null
            } else {
                json!(config_reason.unwrap_or("Screaming Frog CLI was not found"))
            },
        },
        "mcp": {
            "state": "available",
            "version": env!("CARGO_PKG_VERSION"),
            "reason": null,
        },
        "mcp_http_bind": mcp_http_bind_state(MCP_HTTP_HOST, options.mcp_http_port.unwrap_or(MCP_HTTP_PORT)),
        "sqlite": {
            "state": "available",
            "version": rusqlite::version(),
            "locking": if cfg!(unix) {
                json!({"state": "available", "reason": null})
            } else {
                json!({"state": "unavailable", "reason": "OSError"})
            },
            "reason": null,
        },
        "projects_directory": project_directory(options.projects_directory.as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::SystemHost;

    #[test]
    fn project_directory_states() {
        assert_eq!(project_directory(None)["state"], "not_checked");
        assert_eq!(project_directory(Some(""))["state"], "unavailable");
        assert_eq!(
            project_directory(Some("/no/such/projects/dir"))["reason"],
            "the selected projects directory is missing or unreadable"
        );
        let dir = std::env::temp_dir();
        assert_eq!(
            project_directory(Some(&dir.to_string_lossy()))["readable"],
            true
        );
    }

    #[test]
    fn report_names_every_component() {
        let report = doctor(&SystemHost, &DoctorOptions::default());
        assert_eq!(report["format"], DOCTOR_FORMAT);
        for key in [
            "python",
            "playwright",
            "chromium",
            "screaming_frog",
            "mcp",
            "mcp_http_bind",
            "sqlite",
            "projects_directory",
        ] {
            assert!(report.get(key).is_some(), "{key}");
        }
    }

    #[test]
    fn a_held_port_is_busy() {
        let held = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = held.local_addr().expect("addr").port();
        let state = mcp_http_bind_state("127.0.0.1", port);
        assert_eq!(state["state"], "busy");
        assert!(state["reason"]
            .as_str()
            .unwrap_or_default()
            .starts_with("[Errno 48]"));
    }
}

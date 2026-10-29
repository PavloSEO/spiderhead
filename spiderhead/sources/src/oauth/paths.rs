//! Path helpers for OAuth grant and flow files, including `~` expansion.

use super::*;

pub(super) fn env(host: &dyn Host, name: &str) -> String {
    host.var(name)
        .map(|v| pystr::strip(&v).to_owned())
        .unwrap_or_default()
}

/// Home directory of the named user, or `None` for an unknown user.
#[cfg(unix)]
pub(super) fn user_home(user: &str) -> Option<PathBuf> {
    let name = std::ffi::CString::new(user).ok()?;
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr((*entry).pw_dir) };
    Some(PathBuf::from(dir.to_string_lossy().into_owned()))
}

/// Home directory of the current process user; used when `HOME` is unset.
#[cfg(unix)]
pub(super) fn current_user_home() -> Option<PathBuf> {
    let entry = unsafe { libc::getpwuid(libc::getuid()) };
    if entry.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr((*entry).pw_dir) };
    Some(PathBuf::from(dir.to_string_lossy().into_owned()))
}

#[cfg(not(unix))]
pub(super) fn user_home(_user: &str) -> Option<PathBuf> {
    None
}

#[cfg(not(unix))]
pub(super) fn current_user_home() -> Option<PathBuf> {
    std::env::home_dir()
}

/// Expand `~` and `~/` through `HOME` or the user database, and `~user` through the user database;
/// an unresolvable home is an error.
pub(super) fn expand_home(host: &dyn Host, value: &str) -> Result<String, OAuthError> {
    let Some(rest) = value.strip_prefix('~') else {
        return Ok(value.to_owned());
    };
    if rest.is_empty() || rest.starts_with('/') {
        let home = host
            .var("HOME")
            .filter(|v| !v.is_empty())
            .or_else(|| current_user_home().map(|p| p.display().to_string()))
            .ok_or_else(|| OAuthError::Runtime("Could not determine home directory.".to_owned()))?;
        return Ok(format!("{home}{rest}"));
    }
    let (user, tail) = rest.split_once('/').unwrap_or((rest, ""));
    match user_home(user) {
        Some(home) => Ok(format!("{}{}", home.display(), tail)),
        None => Err(OAuthError::Runtime(
            "Could not determine home directory.".to_owned(),
        )),
    }
}

pub(super) fn netloc_parts(netloc: &str) -> (Option<String>, Option<String>, String) {
    let (userinfo, hostport) = match netloc.rsplit_once('@') {
        Some((user, host)) => (Some(user.to_owned()), host),
        None => (None, netloc),
    };
    let (username, password) = match userinfo.as_deref().map(str::to_owned) {
        Some(info) => match info.split_once(':') {
            Some((u, p)) => (Some(u.to_owned()), Some(p.to_owned())),
            None => (Some(info), None),
        },
        None => (None, None),
    };
    let hostname = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default().to_owned()
    } else {
        hostport.split(':').next().unwrap_or_default().to_owned()
    };
    // The host is lowercased, as URL hosts are case-insensitive.
    (username, password, hostname.to_lowercase())
}

/// Points inside the callback commit where a concurrent test can interpose and block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPoint {
    /// After the authorization-code exchange returned, before the commit lock.
    AfterExchange,
    /// Inside the commit lock, before the second state check and grant save.
    InCommit,
}

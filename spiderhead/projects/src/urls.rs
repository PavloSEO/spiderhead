//! URL splitting, joining and fragment removal for project input validation.
//!
//! The split itself is `spiderhead_parse::py_url` (byte-exact, netloc
//! checks included); this module keeps the component accessors used by validation
//! (username/password/hostname/port with the port's `ValueError` behavior) and the
//! defragment helper.

use crate::error::ProjectsError;

/// The five components of a URL plus netloc accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// `scheme` (lowercased).
    pub scheme: String,
    /// `netloc` (verbatim).
    pub netloc: String,
    /// `path`.
    pub path: String,
    /// `query` (without `?`).
    pub query: String,
    /// `fragment` (without `#`).
    pub fragment: String,
}

impl Split {
    /// Username from the netloc.
    pub fn username(&self) -> Option<&str> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        Some(userinfo.split(':').next().unwrap_or(""))
    }

    /// Password from the netloc.
    pub fn password(&self) -> Option<&str> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        userinfo.split_once(':').map(|(_, p)| p)
    }

    /// Hostname: lowercased, brackets stripped for IPv6 literals.
    pub fn hostname(&self) -> Option<String> {
        let host = self.netloc.rsplit('@').next().unwrap_or("");
        let host = if let Some(bracketed) = host.strip_prefix('[') {
            match bracketed.split_once(']') {
                Some((h, _)) => h,
                None => bracketed,
            }
        } else {
            host.split(':').next().unwrap_or("")
        };
        if host.is_empty() {
            None
        } else {
            Some(host.to_lowercase())
        }
    }

    /// Port number; `Err` for a non-numeric or out-of-range (>65535) port.
    pub fn port(&self) -> Result<Option<u16>, ProjectsError> {
        let host = self.netloc.rsplit('@').next().unwrap_or("");
        let port_str = if let Some(bracketed) = host.strip_prefix('[') {
            match bracketed.split_once(']') {
                Some((_, rest)) => rest.strip_prefix(':').unwrap_or(""),
                None => "",
            }
        } else {
            host.split(':').nth(1).unwrap_or("")
        };
        if port_str.is_empty() {
            return Ok(None);
        }
        if port_str.bytes().any(|b| !b.is_ascii_digit()) {
            return Err(crate::error::value(format!(
                "Port could not be cast to integer value as '{port_str}'"
            )));
        }
        let port: i64 = port_str.parse().unwrap_or(i64::MAX);
        if port > 65535 {
            return Err(crate::error::value("Port out of range 0-65535".to_string()));
        }
        Ok(Some(port as u16))
    }
}

/// Split a URL into its components via `spiderhead_parse::py_url`.
pub fn urlsplit(input: &str) -> Result<Split, ProjectsError> {
    let parts = spiderhead_parse::py_url::urlsplit(input)
        .map_err(|e| crate::error::value(e.to_string()))?;
    Ok(Split {
        scheme: parts.scheme,
        netloc: parts.netloc,
        path: parts.path,
        query: parts.query,
        fragment: parts.fragment,
    })
}

/// Join components back into a URL via `spiderhead_parse::py_url`.
pub fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    spiderhead_parse::py_url::urlunsplit(scheme, netloc, path, query, fragment)
}

/// Split off the fragment: returns `(url without fragment, fragment)`.
pub fn urldefrag(input: &str) -> (String, String) {
    let cleaned: String = input
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    match cleaned.split_once('#') {
        Some((base, frag)) => (base.to_owned(), frag.to_owned()),
        None => (cleaned, String::new()),
    }
}

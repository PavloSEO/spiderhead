//! Generic URL splitting with the exact leniency the log store depends on: paths from logs
//! are arbitrary client input, so nothing is normalised and nothing is rejected beyond
//! malformed bracketed hosts.

use unicode_normalization::UnicodeNormalization;

use crate::error::{LogsError, Result};
use crate::net::parse_ip;

/// The five parts of a URL reference; absent parts are empty strings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Split {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

fn scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

fn invalid_ipv6() -> LogsError {
    LogsError::value("Invalid IPv6 URL")
}

fn check_bracketed_host(host: &str) -> Result<()> {
    if host.starts_with('v') {
        let valid = host
            .strip_prefix('v')
            .and_then(|rest| rest.split_once('.'))
            .is_some_and(|(hex, tail)| {
                !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty()
            });
        if !valid {
            return Err(LogsError::value("IPvFuture address is invalid"));
        }
        return Ok(());
    }
    match parse_ip(host) {
        None => Err(LogsError::value(format!(
            "'{host}' does not appear to be an IPv4 or IPv6 address"
        ))),
        Some(std::net::IpAddr::V4(_)) => {
            Err(LogsError::value("An IPv4 address cannot be in brackets"))
        }
        Some(_) => Ok(()),
    }
}

fn check_bracketed_netloc(netloc: &str) -> Result<()> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, tail)| tail);
    let host = if let Some((before, bracketed)) = host_and_port.split_once('[') {
        if !before.is_empty() {
            return Err(invalid_ipv6());
        }
        let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        if !after.is_empty() && !after.starts_with(':') {
            return Err(invalid_ipv6());
        }
        host
    } else {
        host_and_port.split(':').next().unwrap_or("")
    };
    check_bracketed_host(host)
}

fn check_netloc(netloc: &str) -> Result<()> {
    if netloc.is_empty() || netloc.is_ascii() {
        return Ok(());
    }
    let stripped: String = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .collect();
    let folded: String = stripped.nfkc().collect();
    if folded == stripped {
        return Ok(());
    }
    if folded
        .chars()
        .any(|c| matches!(c, '/' | '?' | '#' | '@' | ':'))
    {
        return Err(LogsError::value(format!(
            "netloc '{netloc}' contains invalid characters under NFKC normalization"
        )));
    }
    Ok(())
}

/// Split `url` into scheme, authority, path, query and fragment.
///
/// Leading control characters and spaces are dropped and tabs and line breaks removed
/// anywhere, as browsers do. Unbalanced or malformed bracketed hosts are errors.
pub fn split(url: &str) -> Result<Split> {
    let mut rest: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut out = Split::default();
    if let Some(i) = rest.find(':') {
        let head = &rest[..i];
        if i > 0
            && head.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && head.chars().all(scheme_char)
        {
            out.scheme = head.to_lowercase();
            rest = rest[i + 1..].to_string();
        }
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        out.netloc = after[..end].to_string();
        let tail = after[end..].to_string();
        let has_open = out.netloc.contains('[');
        let has_close = out.netloc.contains(']');
        if has_open != has_close {
            return Err(invalid_ipv6());
        }
        if has_open {
            check_bracketed_netloc(&out.netloc)?;
        }
        rest = tail;
    }
    if let Some((head, fragment)) = rest.split_once('#') {
        out.fragment = fragment.to_string();
        rest = head.to_string();
    }
    if let Some((head, query)) = rest.split_once('?') {
        out.query = query.to_string();
        rest = head.to_string();
    }
    check_netloc(&out.netloc)?;
    out.path = rest;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_the_five_parts() {
        let s = split("HTTPS://Example.com:8080/a/b?x=1#top").unwrap();
        assert_eq!(s.scheme, "https");
        assert_eq!(s.netloc, "Example.com:8080");
        assert_eq!(s.path, "/a/b");
        assert_eq!(s.query, "x=1");
        assert_eq!(s.fragment, "top");
    }

    #[test]
    fn path_only_and_protocol_relative() {
        assert_eq!(split("/catalog/x?y").unwrap().path, "/catalog/x");
        let s = split("//evil.test/x").unwrap();
        assert_eq!((s.netloc.as_str(), s.path.as_str()), ("evil.test", "/x"));
    }

    #[test]
    fn unbalanced_brackets_are_errors() {
        assert!(split("http://[::1/x").is_err());
        assert!(split("http://[::1]/x").is_ok());
        assert!(split("http://[1.2.3.4]/x").is_err());
    }
}

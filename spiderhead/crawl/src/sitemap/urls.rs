//! URL helpers and crawl ceilings shared by the sitemap crawl and the audit.

use serde_json::{json, Value};
use spiderhead_parse::py_url;

/// `MAX_SITEMAPS`: retained sitemap-document ceiling per crawl.
pub const MAX_SITEMAPS: usize = 5000;
/// `MAX_URLS`: the crawler-wide retained-member ceiling.
pub const MAX_URLS: usize = 1_000_000;
/// `MAX_XML_BYTES`: document cap, compressed or decompressed.
pub const MAX_XML_BYTES: usize = 12 * 1024 * 1024;
/// The one "too large" message, raised whichever stage catches the overflow.
pub const TOO_LARGE_MSG: &str = "Response too large (> 12582912 bytes)";
/// `TIMEOUT_S`: one sitemap document fetch.
pub const TIMEOUT_SECONDS: f64 = 25.0;

/// `strip_www`: the host without a leading `www.` label.
pub fn strip_www(host: &str) -> String {
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

/// The host part of a netloc: userinfo dropped, IPv6 brackets stripped.
pub(super) fn netloc_host(netloc: &str) -> String {
    let host_part = netloc.rsplit('@').next().unwrap_or(netloc);
    if let Some(inner) = host_part.strip_prefix('[') {
        return inner.split(']').next().unwrap_or(inner).to_lowercase();
    }
    match host_part.rfind(':') {
        Some(colon)
            if !host_part[colon + 1..].is_empty() && !host_part[colon + 1..].contains(']') =>
        {
            host_part[..colon].to_lowercase()
        }
        _ => host_part.to_lowercase(),
    }
}

/// The explicit port of a netloc: `None` when absent, `Err` when it is outside 0-65535.
pub(super) fn netloc_port(netloc: &str) -> Result<Option<u16>, String> {
    let host_part = netloc.rsplit('@').next().unwrap_or(netloc);
    let colon = if host_part.starts_with('[') {
        host_part
            .find(']')
            .and_then(|end| host_part[end..].find(':').map(|rel| end + rel))
    } else {
        host_part.rfind(':')
    };
    let Some(colon) = colon else { return Ok(None) };
    let raw = &host_part[colon + 1..];
    if raw.is_empty() {
        return Ok(None);
    }
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Port could not be cast to integer value".to_owned());
    }
    // Leading zeros are allowed; the value is bounded by u32 digits.
    match raw.parse::<u64>() {
        Ok(value) if value <= 65535 => Ok(Some(value as u16)),
        _ => Err("Port out of range 0-65535".to_owned()),
    }
}

/// `normalize_url`: lowercase host, default port dropped, fragment removed, trailing slashes stripped.
/// `Err` for an invalid URL or a port outside 0-65535.
pub fn normalize_url(url_str: &str) -> Result<String, String> {
    let parts = py_url::urlsplit(url_str.trim()).map_err(|_| invalid_url(url_str))?;
    if parts.scheme.is_empty() || parts.netloc.is_empty() {
        return Err(invalid_url(url_str));
    }
    let port = netloc_port(&parts.netloc)?;
    let scheme = parts.scheme.to_lowercase();
    let host = netloc_host(&parts.netloc);
    let port = match port {
        Some(p) if (scheme == "https" && p == 443) || (scheme == "http" && p == 80) => None,
        other => other,
    };
    let mut netloc = host.clone();
    if let Some(port) = port {
        netloc = format!("{host}:{port}");
    }
    let userinfo = parts.netloc.split('@').next().unwrap_or("");
    if parts.netloc.contains('@') && !userinfo.is_empty() {
        netloc = format!("{userinfo}@{netloc}");
    }
    let mut path = parts.path.clone();
    if path.len() > 1 && path.ends_with('/') {
        while path.len() > 1 && path.ends_with('/') {
            path.pop();
        }
    }
    Ok(py_url::urlunsplit(
        &scheme,
        &netloc,
        &path,
        &parts.query,
        "",
    ))
}

pub(super) fn invalid_url(url_str: &str) -> String {
    format!("Invalid URL: '{url_str}'")
}

/// `classify_url_type`: `"folder"` or `"finalUrl"`; the root path is a folder,
/// a dot in the leaf marks a final URL, malformed URLs default to `"finalUrl"`.
pub fn classify_url_type(url_str: &str) -> String {
    let Ok(parts) = py_url::urlsplit(url_str) else {
        return "finalUrl".to_owned();
    };
    if parts.scheme.is_empty() || parts.netloc.is_empty() {
        return "finalUrl".to_owned();
    }
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    if path == "/" {
        return "folder".to_owned();
    }
    let leaf = path.split('/').rfind(|s| !s.is_empty()).unwrap_or("");
    if leaf.contains('.') {
        "finalUrl".to_owned()
    } else {
        "folder".to_owned()
    }
}

/// `detect_host_policy`: the www vs non-www split; ties favour www.
pub fn detect_host_policy(urls: &[Value]) -> Value {
    let mut www = 0i64;
    let mut non_www = 0i64;
    let mut by_host: indexmap::IndexMap<String, i64> = indexmap::IndexMap::new();
    for url in urls {
        let Ok(parts) = py_url::urlsplit(url.get("loc").and_then(Value::as_str).unwrap_or(""))
        else {
            continue;
        };
        let host = netloc_host(&parts.netloc);
        if host.is_empty() {
            continue;
        }
        if host.starts_with("www.") {
            www += 1;
        } else {
            non_www += 1;
        }
        *by_host.entry(host).or_insert(0) += 1;
    }
    let dominant = if www >= non_www { "www" } else { "non_www" };
    let mixed = www > 0 && non_www > 0;
    json!({
        "dominant": dominant,
        "mixed": mixed,
        "counts": {"www": www, "non_www": non_www},
        "by_host": by_host.into_iter().map(|(host, count)| json!([host, count])).collect::<Vec<_>>(),
    })
}

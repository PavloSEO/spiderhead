//! Explicit HTTP forward-proxy resolution.
//!
//! The proxy is named as a URL, or as `env:VARIABLE` so its credentials never sit in a config
//! file. The resolved route carries a credential-free identity. The httpx proxy object is not
//! part of this crate: the transport layer does not exist yet.

use super::is_public_address;
use crate::host::Host;

/// A vetted proxy route. `identity` never carries credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRoute {
    /// `http://host:port` in lowercase, without credentials.
    pub identity: String,
    /// Whether the route supplies proxy credentials.
    pub authenticated: bool,
}

/// Decode `%XX` escapes, replacing invalid UTF-8 with U+FFFD.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let pair = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(pair, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `%` not followed by two hex digits.
fn has_bad_percent(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(index, &byte)| {
        byte == b'%'
            && !(index + 2 < bytes.len()
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit())
    })
}

fn is_control_or_space(c: char) -> bool {
    (c as u32) < 33 || c as u32 == 127
}

/// Name inside `env:NAME`, if the whole value is that reference.
fn env_reference(value: &str) -> Option<&str> {
    let name = value.strip_prefix("env:")?;
    let mut chars = name.chars();
    let first = chars.next()?;
    let valid = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some(name)
}

/// Resolve and pin an explicitly configured HTTP forward proxy. `value` is `""` (no proxy),
/// an `http://` URL, or `env:VARIABLE`. Returns `Ok(None)` for the empty value.
pub fn resolve_proxy_route(
    value: &str,
    allow_private: bool,
    host: &dyn Host,
) -> Result<Option<ProxyRoute>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    let reference = env_reference(value);
    if reference.is_none() && value.starts_with("env:") {
        return Err("http.proxy needs a valid env:VARIABLE reference".into());
    }
    let raw = match reference {
        Some(name) => host.var(name).unwrap_or_default(),
        None => value.to_owned(),
    };
    if raw.is_empty() {
        return Err("http.proxy environment reference is missing or empty".into());
    }
    if raw.chars().any(is_control_or_space) {
        return Err("http.proxy URL contains whitespace or control characters".into());
    }
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| "http.proxy supports only an http:// forward proxy".to_string())?;
    if !scheme.eq_ignore_ascii_case("http") {
        return Err("http.proxy supports only an http:// forward proxy".into());
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    if !(tail.is_empty() || tail == "/") {
        return Err("http.proxy URL cannot have a path, query, or fragment".into());
    }
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((info, hp)) => (Some(info), hp),
        None => (None, authority),
    };
    let (host_name, port) = split_host_port(hostport)
        .ok_or_else(|| "http.proxy requires a host and explicit port".to_string())?;
    let auth = match userinfo {
        None => None,
        Some(info) => {
            if reference.is_none() {
                return Err(
                    "http.proxy inline authentication is forbidden; use env:VARIABLE".into(),
                );
            }
            let (user, pass) = info
                .split_once(':')
                .filter(|(u, p)| !u.is_empty() && !p.is_empty())
                .ok_or_else(|| {
                    "http.proxy authentication needs both username and password".to_string()
                })?;
            if has_bad_percent(user) || has_bad_percent(pass) {
                return Err("http.proxy authentication has invalid percent encoding".into());
            }
            let user = percent_decode(user);
            let pass = percent_decode(pass);
            if user.chars().chain(pass.chars()).any(is_control_or_space) {
                return Err("http.proxy authentication contains control characters".into());
            }
            if user.contains(':') {
                return Err("http.proxy username cannot contain a colon".into());
            }
            Some((user, pass))
        }
    };
    let addresses = host
        .resolve(&host_name, port)
        .map_err(|_| "http.proxy host could not be resolved safely".to_string())?;
    if addresses.is_empty() {
        return Err("http.proxy host could not be resolved safely".into());
    }
    if !allow_private
        && addresses
            .iter()
            .any(|a| !is_public_address(&a.ip().to_string()))
    {
        return Err(
            "http.proxy resolves to a private address; set http.proxy_allow_private=true".into(),
        );
    }
    Ok(Some(ProxyRoute {
        identity: format!("http://{}:{port}", host_name.to_lowercase()),
        authenticated: auth.is_some(),
    }))
}

/// Split `host:port`, accepting bracketed IPv6. Returns `None` without an explicit valid port.
fn split_host_port(hostport: &str) -> Option<(String, u16)> {
    let (host_name, port_text) = if let Some(stripped) = hostport.strip_prefix('[') {
        let (inner, after) = stripped.split_once(']')?;
        (inner.to_owned(), after.strip_prefix(':')?)
    } else {
        let (h, p) = hostport.rsplit_once(':')?;
        (h.to_owned(), p)
    };
    if host_name.is_empty() {
        return None;
    }
    let port: u16 = port_text.parse().ok().filter(|p| *p >= 1)?;
    Some((host_name, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;
    use std::net::SocketAddr;

    fn host_with_dns(name: &str, ip: &str) -> FakeHost {
        let mut h = FakeHost::default();
        h.dns.insert(
            name.to_owned(),
            vec![SocketAddr::new(ip.parse().unwrap(), 3128)],
        );
        h
    }

    // Behaviour of resolve_proxy_route: an empty value means no proxy.
    #[test]
    fn empty_value_means_no_proxy() {
        assert_eq!(
            resolve_proxy_route("", false, &FakeHost::default()),
            Ok(None)
        );
    }

    // Behaviour of resolve_proxy_route: a public proxy yields a credential-free identity.
    #[test]
    fn public_proxy_yields_identity_without_credentials() {
        let h = host_with_dns("Proxy.Example.test", "8.8.8.8");
        let route = resolve_proxy_route("http://Proxy.Example.test:3128/", false, &h)
            .unwrap()
            .unwrap();
        assert_eq!(route.identity, "http://proxy.example.test:3128");
        assert!(!route.authenticated);
    }

    // Behaviour of resolve_proxy_route: private proxy refused without opt-in.
    #[test]
    fn private_proxy_needs_explicit_opt_in() {
        let h = host_with_dns("p.example.test", "10.0.0.9");
        assert!(resolve_proxy_route("http://p.example.test:3128", false, &h).is_err());
        assert!(resolve_proxy_route("http://p.example.test:3128", true, &h).is_ok());
    }

    // Behaviour of resolve_proxy_route: inline credentials are forbidden; env reference works.
    #[test]
    fn credentials_must_come_from_environment() {
        let mut h = host_with_dns("p.example.test", "8.8.8.8");
        assert!(resolve_proxy_route("http://u:p@p.example.test:3128", false, &h).is_err());
        h.env.insert(
            "PROXY_URL".into(),
            "http://u:p%40x@p.example.test:3128".into(),
        );
        let route = resolve_proxy_route("env:PROXY_URL", false, &h)
            .unwrap()
            .unwrap();
        assert!(route.authenticated);
        assert_eq!(route.identity, "http://p.example.test:3128");
    }

    // Behaviour of resolve_proxy_route: https proxies, paths and missing ports are refused.
    #[test]
    fn refuses_unsupported_forms() {
        let h = host_with_dns("p.example.test", "8.8.8.8");
        assert!(resolve_proxy_route("https://p.example.test:3128", false, &h).is_err());
        assert!(resolve_proxy_route("http://p.example.test/x", false, &h).is_err());
        assert!(resolve_proxy_route("http://p.example.test", false, &h).is_err());
    }
}

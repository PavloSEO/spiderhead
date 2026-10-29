//! Address classification and target validation: which resolved addresses may be reached.
//!
//! Covers the private-network opt-in, public-address checks, connection pinning and URL validation.
//! Routability follows the IANA special-purpose address registries.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use url::Url;

use super::{PRIVATE_HOST_ALLOWLIST_ENV, PRIVATE_NETWORK_ENV};
use crate::envvar::read_env;
use crate::host::Host;
use crate::remote_policy::RemoteEgressPolicy;

/// IPv4 blocks that are not global: private ranges plus the shared 100.64/10 range,
/// as (network, prefix length).
const V4_NON_GLOBAL: &[(u32, u32)] = &[
    (0x0000_0000, 8),  // 0.0.0.0/8
    (0x0A00_0000, 8),  // 10.0.0.0/8
    (0x6440_0000, 10), // 100.64.0.0/10 shared address space
    (0x7F00_0000, 8),  // 127.0.0.0/8
    (0xA9FE_0000, 16), // 169.254.0.0/16
    (0xAC10_0000, 12), // 172.16.0.0/12
    (0xC000_0000, 29), // 192.0.0.0/29
    (0xC000_00AA, 31), // 192.0.0.170/31
    (0xC000_0200, 24), // 192.0.2.0/24
    (0xC0A8_0000, 16), // 192.168.0.0/16
    (0xC612_0000, 15), // 198.18.0.0/15
    (0xC633_6400, 24), // 198.51.100.0/24
    (0xCB00_7100, 24), // 203.0.113.0/24
    (0xF000_0000, 4),  // 240.0.0.0/4
    (0xFFFF_FFFF, 32), // 255.255.255.255/32
];

/// IPv4 addresses that are global despite falling inside a private block above.
const V4_GLOBAL_EXCEPTIONS: &[u32] = &[0xC000_0009, 0xC000_000A]; // 192.0.0.9, 192.0.0.10

/// IPv6 blocks that are not global, as (network, prefix length).
const V6_NON_GLOBAL: &[(u128, u32)] = &[
    (1, 128),                                        // ::1/128
    (0, 128),                                        // ::/128
    (0xFFFF_0000_0000, 96),                          // ::ffff:0:0/96
    (0x0064_FF9B_0001_0000_0000_0000_0000_0000, 48), // 64:ff9b:1::/48
    (0x0100_0000_0000_0000_0000_0000_0000_0000, 64), // 100::/64
    (0x2001_0000_0000_0000_0000_0000_0000_0000, 23), // 2001::/23
    (0x2001_0DB8_0000_0000_0000_0000_0000_0000, 32), // 2001:db8::/32
    (0x2002_0000_0000_0000_0000_0000_0000_0000, 16), // 2002::/16
    (0x2620_004F_8000_0000_0000_0000_0000_0000, 48), // 2620:4f:8000::/48
    (0xFC00_0000_0000_0000_0000_0000_0000_0000, 7),  // fc00::/7
    (0xFE80_0000_0000_0000_0000_0000_0000_0000, 10), // fe80::/10
];

/// IPv6 addresses that are global despite falling inside a private block above.
const V6_GLOBAL_EXCEPTIONS: &[(u128, u32)] = &[
    (0x2001_0001_0000_0000_0000_0000_0000_0001, 128), // 2001:1::1
    (0x2001_0001_0000_0000_0000_0000_0000_0002, 128), // 2001:1::2
    (0x2001_0003_0000_0000_0000_0000_0000_0000, 32),  // 2001:3::/32
    (0x2001_0004_0112_0000_0000_0000_0000_0000, 48),  // 2001:4:112::/48
    (0x2001_0020_0000_0000_0000_0000_0000_0000, 28),  // 2001:20::/28
    (0x2001_0030_0000_0000_0000_0000_0000_0000, 28),  // 2001:30::/28
];

/// Translated forms whose own scope says nothing about the destination.
const V6_TRANSLATED: &[(u128, u32)] = &[
    (0x0064_FF9B_0000_0000_0000_0000_0000_0000, 96), // RFC 6052 NAT64
    (0x0064_FF9B_0001_0000_0000_0000_0000_0000, 48), // RFC 8215 local-use NAT64
    (0x2002_0000_0000_0000_0000_0000_0000_0000, 16), // 6to4
    (0x0000_0000_0000_0000_0000_FFFF_0000_0000, 96), // IPv4-mapped
    (0, 96),                                         // IPv4-compatible
];

fn in_v4(addr: u32, net: u32, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    addr & mask == net & mask
}

fn in_v6(addr: u128, net: u128, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    addr & mask == net & mask
}

fn is_global_v4(addr: Ipv4Addr) -> bool {
    let value = u32::from(addr);
    if V4_GLOBAL_EXCEPTIONS.contains(&value) {
        return true;
    }
    !V4_NON_GLOBAL
        .iter()
        .any(|&(net, prefix)| in_v4(value, net, prefix))
}

fn is_global_v6(addr: Ipv6Addr) -> bool {
    let value = u128::from(addr);
    if V6_GLOBAL_EXCEPTIONS
        .iter()
        .any(|&(net, prefix)| in_v6(value, net, prefix))
    {
        return true;
    }
    !V6_NON_GLOBAL
        .iter()
        .any(|&(net, prefix)| in_v6(value, net, prefix))
}

/// The IPv4 address that a translated IPv6 address actually reaches.
fn embedded_ipv4(addr: Ipv6Addr) -> Ipv4Addr {
    let octets = addr.octets();
    if in_v6(u128::from(addr), V6_TRANSLATED[2].0, V6_TRANSLATED[2].1) {
        // 6to4 carries the IPv4 address in bytes 2..6.
        Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5])
    } else {
        Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15])
    }
}

/// Whether a textual address is globally routable; unparsable text is not public.
pub fn is_public_address(value: &str) -> bool {
    let text = value.split('%').next().unwrap_or(value);
    match text.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => is_global_v4(v4),
        Ok(IpAddr::V6(v6)) => {
            let bits = u128::from(v6);
            if V6_TRANSLATED
                .iter()
                .any(|&(net, prefix)| in_v6(bits, net, prefix))
            {
                is_global_v4(embedded_ipv4(v6))
            } else {
                is_global_v6(v6)
            }
        }
        Err(_) => false,
    }
}

/// Whether an address lies in RFC 1918 space or IPv6 unique-local space (`fc00::/7`), the only
/// ranges an allowlisted staging host may resolve into.
pub fn allows_private_range(value: &str) -> bool {
    let text = value.split('%').next().unwrap_or(value);
    match text.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            let bits = u32::from(v4);
            [(0x0A00_0000, 8), (0xAC10_0000, 12), (0xC0A8_0000, 16)]
                .iter()
                .any(|&(net, prefix)| in_v4(bits, net, prefix))
        }
        Ok(IpAddr::V6(v6)) => in_v6(u128::from(v6), 0xFC00_0000_0000_0000_0000_0000_0000_0000, 7),
        Err(_) => false,
    }
}

/// Whether `SEOHEAD_ALLOW_PRIVATE_NETWORKS` (or its `SPIDERHEAD_` spelling) is truthy.
pub fn private_networks_enabled(host: &dyn Host) -> bool {
    matches!(
        read_env(host, PRIVATE_NETWORK_ENV)
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Hostnames explicitly allowed to resolve privately (`SEOHEAD_ALLOW_PRIVATE_HOSTS`).
pub fn allowed_private_hosts(host: &dyn Host) -> BTreeSet<String> {
    read_env(host, PRIVATE_HOST_ALLOWLIST_ENV)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.trim_end_matches('.').to_lowercase())
        .collect()
}

/// Whether `host` may resolve to a private or otherwise non-public address.
fn private_target_allowed(
    host_name: &str,
    env: &dyn Host,
    policy: Option<&RemoteEgressPolicy>,
) -> bool {
    if let Some(policy) = policy {
        return policy.allows_private_host(host_name);
    }
    private_networks_enabled(env)
        || allowed_private_hosts(env).contains(&host_name.trim_end_matches('.').to_lowercase())
}

/// A vetted address to connect to, keeping the original host for Host and SNI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedTarget {
    /// URL rewritten to the literal address.
    pub url: String,
    /// Original `Host` authority to send.
    pub host_header: String,
    /// Hostname used for certificate verification.
    pub sni_hostname: String,
}

/// Resolve once and return the vetted socket addresses for a direct connection.
/// `policy` replaces the environment-based private opt-in.
pub fn resolve_socket_addresses(
    host_name: &str,
    port: u16,
    env: &dyn Host,
    policy: Option<&RemoteEgressPolicy>,
) -> Result<Vec<SocketAddr>, String> {
    let records = match env.resolve(host_name, port) {
        Ok(records) if !records.is_empty() => records,
        Ok(_) | Err(_) => {
            return Err(if policy.is_some() {
                "hostname could not be resolved safely".into()
            } else {
                format!("hostname could not be resolved safely: {host_name}")
            });
        }
    };
    if let Some(policy) = policy {
        let publicness: BTreeSet<bool> = records
            .iter()
            .map(|r| is_public_address(&r.ip().to_string()))
            .collect();
        if publicness.len() > 1 {
            return Err("mixed public and private DNS answers blocked".into());
        }
        if records.iter().any(|r| {
            let ip = r.ip().to_string();
            !is_public_address(&ip) && !policy.allows_private_address(host_name, &ip)
        }) {
            return Err("private or non-public network target blocked".into());
        }
    } else if !private_target_allowed(host_name, env, None)
        && records
            .iter()
            .any(|r| !is_public_address(&r.ip().to_string()))
    {
        return Err(format!(
            "private or non-public network target blocked; set {PRIVATE_NETWORK_ENV}=1 to \
             authorize every private target, or add {host_name:?} to {PRIVATE_HOST_ALLOWLIST_ENV} \
             to authorize only this one"
        ));
    }
    let mut unique: Vec<SocketAddr> = Vec::new();
    for record in records {
        if !unique.contains(&record) {
            unique.push(record);
        }
    }
    Ok(unique)
}

/// Rewrite a URL to connect to a vetted IPv4 (else first) address, keeping the hostname for
/// Host and SNI.
pub fn pinned_target(
    url: &str,
    env: &dyn Host,
    policy: Option<&RemoteEgressPolicy>,
) -> Result<PinnedTarget, String> {
    let parsed = Url::parse(url).map_err(|_| format!("no host to pin in {url:?}"))?;
    let host_name = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| format!("no host to pin in {url:?}"))?
        .to_owned();
    let explicit_port = parsed.port();
    let port = explicit_port.unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
    let addresses = resolve_socket_addresses(&host_name, port, env, policy)?;
    let selected = addresses
        .iter()
        .find(|addr| addr.is_ipv4())
        .or(addresses.first())
        .ok_or_else(|| "hostname could not be resolved safely".to_string())?;
    let literal = match selected.ip() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    let netloc = match explicit_port {
        Some(p) => format!("{literal}:{p}"),
        None => literal,
    };
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let query = parsed.query().map(|q| format!("?{q}")).unwrap_or_default();
    let host_header = match explicit_port {
        Some(p) => format!("{host_name}:{p}"),
        None => host_name.clone(),
    };
    Ok(PinnedTarget {
        url: format!("{}://{netloc}{path}{query}", parsed.scheme()),
        host_header,
        sni_hostname: host_name,
    })
}

/// Validate an HTTP(S) URL and block private networks by default.
///
/// Embedded credentials are refused. Hostnames are resolved and every answer must be global
/// unless the environment or the policy authorizes it.
pub fn validate_url(
    url: &str,
    env: &dyn Host,
    policy: Option<&RemoteEgressPolicy>,
) -> Result<String, String> {
    let value = url.trim().to_owned();
    let parsed = Url::parse(&value).map_err(|error| {
        if policy.is_some() {
            "invalid URL".to_owned()
        } else {
            format!("invalid URL: {error}")
        }
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http:// and https:// URLs are supported".into());
    }
    let Some(host_name) = parsed.host_str().filter(|h| !h.is_empty()) else {
        return Err("URL must include a hostname".into());
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("embedded URL credentials are not supported".into());
    }
    if policy.is_none() && private_networks_enabled(env) {
        return Ok(value);
    }
    let host_name = host_name.trim_end_matches('.').to_lowercase();
    if (host_name == "localhost" || host_name.ends_with(".localhost"))
        && !private_target_allowed(&host_name, env, policy)
    {
        return Err(if policy.is_some() {
            "private-network target blocked".into()
        } else {
            format!(
                "private-network target blocked; set {PRIVATE_NETWORK_ENV}=1 to authorize every \
                 private target, or add {host_name:?} to {PRIVATE_HOST_ALLOWLIST_ENV} to \
                 authorize only this one"
            )
        });
    }
    let port = parsed
        .port()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
    resolve_socket_addresses(&host_name, port, env, policy)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;
    use std::net::SocketAddr;

    fn sock(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 443)
    }

    fn dns(host_name: &str, ips: &[&str]) -> FakeHost {
        let mut h = FakeHost::default();
        h.dns.insert(
            host_name.to_owned(),
            ips.iter().map(|ip| sock(ip)).collect(),
        );
        h
    }

    // Loopback, private, shared, multicast and documentation ranges are not public; ordinary addresses are.
    #[test]
    fn classifies_ipv4_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.1.1",
            "203.0.113.5",
        ] {
            assert!(!is_public_address(ip), "{ip} should not be public");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "192.0.0.9", "192.0.0.10"] {
            assert!(is_public_address(ip), "{ip} should be public");
        }
        assert!(!is_public_address("not-an-ip"));
    }

    // A NAT64 wrapper around loopback must not pass the guard.
    #[test]
    fn translated_ipv6_is_judged_by_its_embedded_ipv4() {
        assert!(!is_public_address("64:ff9b::7f00:1"));
        assert!(!is_public_address("::ffff:127.0.0.1"));
        assert!(is_public_address("64:ff9b::808:808"));
        assert!(!is_public_address("::1"));
        assert!(!is_public_address("fc00::1"));
        assert!(is_public_address("2606:4700:4700::1111"));
    }

    // A private DNS answer is blocked without an opt-in.
    #[test]
    fn private_answer_is_blocked_without_opt_in() {
        let h = dns("staging.example.test", &["10.0.0.5"]);
        assert!(validate_url("https://staging.example.test/", &h, None).is_err());
    }

    // The scoped alternative: one named host may resolve privately.
    #[test]
    fn allowlisted_private_host_passes() {
        let mut h = dns("staging.example.test", &["10.0.0.5"]);
        h.env.insert(
            "SEOHEAD_ALLOW_PRIVATE_HOSTS".into(),
            "Staging.Example.test.".into(),
        );
        assert!(validate_url("https://staging.example.test/", &h, None).is_ok());
    }

    // Mixed public and private answers are refused whatever the opt-in.
    #[test]
    fn mixed_answers_are_refused_under_policy() {
        let h = dns("mixed.example.test", &["8.8.8.8", "10.0.0.5"]);
        let policy = RemoteEgressPolicy::new(
            "p1",
            ["mixed.example.test".to_owned()],
            1000,
            20_000,
            4,
            0.5,
            false,
        )
        .unwrap();
        let error =
            resolve_socket_addresses("mixed.example.test", 443, &h, Some(&policy)).unwrap_err();
        assert_eq!(error, "mixed public and private DNS answers blocked");
    }

    // Embedded credentials and non-HTTP schemes are refused.
    #[test]
    fn url_guard_refuses_credentials_and_non_http_schemes() {
        let h = dns("ok.example.test", &["8.8.8.8"]);
        assert!(validate_url("https://user:pw@ok.example.test/", &h, None).is_err());
        assert!(validate_url("ftp://ok.example.test/", &h, None).is_err());
        assert!(validate_url("https://ok.example.test/", &h, None).is_ok());
    }

    // localhost names are blocked by name even if DNS would answer publicly.
    #[test]
    fn localhost_name_is_blocked() {
        let h = dns("app.localhost", &["8.8.8.8"]);
        assert!(validate_url("http://app.localhost/", &h, None).is_err());
    }

    // Pinning rewrites to the vetted IPv4 and keeps the hostname for Host and SNI.
    #[test]
    fn pinned_target_connects_to_vetted_address_and_keeps_hostname() {
        let h = dns("ok.example.test", &["8.8.8.8"]);
        let pinned = pinned_target("https://ok.example.test/a?b=1", &h, None).unwrap();
        assert_eq!(pinned.url, "https://8.8.8.8/a?b=1");
        assert_eq!(pinned.host_header, "ok.example.test");
        assert_eq!(pinned.sni_hostname, "ok.example.test");
    }
}

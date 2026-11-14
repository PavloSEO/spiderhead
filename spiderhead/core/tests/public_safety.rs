//! Ports of the URL and address guard tests in the reference suite.
//!
//! reference monkeypatches `socket.getaddrinfo` and `os.environ`; here the same DNS answers and
//! environment come through an injected [`FakeHost`], so the inputs and expected values match
//! reference. Tests of the optimizer, the auth proxy and the redirect hook are listed in the slice
//! report, not here.

use std::net::{IpAddr, SocketAddr};

use spiderhead_core::host::FakeHost;
use spiderhead_core::net::{
    allowed_private_hosts, resolve_socket_addresses, validate_url, PRIVATE_HOST_ALLOWLIST_ENV,
    PRIVATE_NETWORK_ENV,
};

fn addr(text: &str, port: u16) -> SocketAddr {
    SocketAddr::new(text.parse::<IpAddr>().unwrap(), port)
}

/// A host whose DNS answers each name with one address on port 443.
fn host_with_dns(entries: &[(&str, &str)]) -> FakeHost {
    let mut host = FakeHost::default();
    for (name, ip) in entries {
        host.dns
            .entry((*name).to_owned())
            .or_default()
            .push(addr(ip, 443));
    }
    host
}

/// Same as [`host_with_dns`] plus one environment variable.
fn host_with_env(entries: &[(&str, &str)], name: &str, value: &str) -> FakeHost {
    let mut host = host_with_dns(entries);
    host.env.insert(name.to_owned(), value.to_owned());
    host
}

#[test]
fn private_networks_are_blocked_by_default() {
    let host = host_with_dns(&[
        ("127.0.0.1", "127.0.0.1"),
        ("169.254.169.254", "169.254.169.254"),
    ]);
    let error = validate_url("http://127.0.0.1/admin", &host, None).unwrap_err();
    assert!(error.contains("private"), "{error}");
    let error = validate_url("http://169.254.169.254/latest/meta-data", &host, None).unwrap_err();
    assert!(error.contains("private"), "{error}");
}

#[test]
fn private_networks_require_explicit_opt_in() {
    let host = host_with_env(&[], PRIVATE_NETWORK_ENV, "1");
    assert_eq!(
        validate_url("http://127.0.0.1:8000/", &host, None).unwrap(),
        "http://127.0.0.1:8000/"
    );
}

#[test]
fn socket_resolution_requires_explicit_private_opt_in() {
    let host = host_with_dns(&[("internal.example", "127.0.0.1")]);
    let error = resolve_socket_addresses("internal.example", 443, &host, None).unwrap_err();
    assert!(error.contains("private or non-public"), "{error}");

    let opted_in = host_with_env(
        &[("internal.example", "127.0.0.1")],
        PRIVATE_NETWORK_ENV,
        "1",
    );
    assert_eq!(
        resolve_socket_addresses("internal.example", 443, &opted_in, None).unwrap(),
        vec![addr("127.0.0.1", 443)]
    );
}

#[test]
fn resolve_socket_addresses_honours_the_named_host_allowlist() {
    let dns = [
        ("staging.internal", "10.0.0.9"),
        ("other-internal.example", "10.0.0.9"),
    ];
    let host = host_with_dns(&dns);
    let error = resolve_socket_addresses("staging.internal", 443, &host, None).unwrap_err();
    assert!(error.contains("private or non-public"), "{error}");

    let allowlisted = host_with_env(&dns, PRIVATE_HOST_ALLOWLIST_ENV, "staging.internal");
    assert_eq!(
        resolve_socket_addresses("staging.internal", 443, &allowlisted, None).unwrap(),
        vec![addr("10.0.0.9", 443)]
    );
    // The obvious bypass: a different host resolving to the same private address stays blocked.
    let error =
        resolve_socket_addresses("other-internal.example", 443, &allowlisted, None).unwrap_err();
    assert!(error.contains("private or non-public"), "{error}");
}

#[test]
fn allowed_private_hosts_permits_only_the_named_host() {
    let host = host_with_env(
        &[("staging.internal", "10.0.0.9")],
        PRIVATE_HOST_ALLOWLIST_ENV,
        "staging.internal",
    );
    assert_eq!(
        validate_url("https://staging.internal/", &host, None).unwrap(),
        "https://staging.internal/"
    );
}

#[test]
fn allowed_private_hosts_does_not_permit_a_different_private_host() {
    let host = host_with_env(
        &[
            ("staging.internal", "10.0.0.9"),
            ("other-internal.example", "10.0.0.9"),
        ],
        PRIVATE_HOST_ALLOWLIST_ENV,
        "staging.internal",
    );
    let error = validate_url("https://other-internal.example/", &host, None).unwrap_err();
    assert!(error.contains("private"), "{error}");
}

#[test]
fn allowed_private_hosts_matching_is_exact_not_a_suffix() {
    let host = host_with_env(
        &[("evil.staging.internal", "10.0.0.9")],
        PRIVATE_HOST_ALLOWLIST_ENV,
        "staging.internal",
    );
    let error = validate_url("https://evil.staging.internal/", &host, None).unwrap_err();
    assert!(error.contains("private"), "{error}");
}

#[test]
fn allowed_private_hosts_is_empty_by_default() {
    assert!(allowed_private_hosts(&FakeHost::default()).is_empty());
}

#[test]
fn url_credentials_are_always_rejected() {
    let host = host_with_env(&[], PRIVATE_NETWORK_ENV, "1");
    let error = validate_url("https://user:password@example.com/", &host, None).unwrap_err();
    assert!(error.contains("credentials"), "{error}");
}

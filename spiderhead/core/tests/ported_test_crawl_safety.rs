//! subset. Robots parsing and crawl-driven cases are deferred (see the slice report).

use std::net::{IpAddr, SocketAddr};

use spiderhead_core::host::FakeHost;
use spiderhead_core::net::{is_public_address, pinned_target, PRIVATE_HOST_ALLOWLIST_ENV};

/// Build a fake resolver from `(host, ip)` pairs, all on port 443.
fn host(dns: &[(&str, &str)]) -> FakeHost {
    let mut fake = FakeHost::default();
    for (name, ip) in dns {
        let addr: IpAddr = ip.parse().expect("test address literal");
        fake.dns
            .entry((*name).to_owned())
            .or_default()
            .push(SocketAddr::new(addr, 443));
    }
    fake
}

#[test]
fn translated_addresses_are_judged_by_where_they_land() {
    let cases: &[(&str, bool)] = &[
        ("8.8.8.8", true),
        ("2001:4860:4860::8888", true),
        ("10.0.0.5", false),
        ("127.0.0.1", false),
        ("169.254.169.254", false),
        ("::ffff:127.0.0.1", false),
        // NAT64 and 6to4 wrappers are judged by the IPv4 address they reach.
        ("64:ff9b::7f00:1", false),
        ("64:ff9b::a00:5", false),
        ("64:ff9b::a9fe:a9fe", false),
        ("64:ff9b:1::a9fe:a9fe", false),
        ("2002:7f00:1::", false),
        ("64:ff9b::808:808", true),
        ("2002:0808:0808::", true),
    ];
    for (address, expected) in cases {
        assert_eq!(is_public_address(address), *expected, "address {address}");
    }
}

#[test]
fn a_malformed_address_is_not_public() {
    assert!(!is_public_address("not-an-address"));
}

#[test]
fn a_request_is_pinned_to_the_address_that_was_vetted() {
    let fake = host(&[("example.com", "8.8.8.8")]);
    let pinned = pinned_target("https://example.com/path?a=1", &fake, None).expect("pinned");
    assert_eq!(pinned.host_header, "example.com");
    assert_eq!(pinned.sni_hostname, "example.com");
    assert!(!pinned.url.contains("example.com"), "url: {}", pinned.url);
    assert!(pinned.url.ends_with("/path?a=1"), "url: {}", pinned.url);
}

#[test]
fn pinning_preserves_a_non_default_port() {
    let fake = host(&[("example.com", "8.8.8.8")]);
    let pinned = pinned_target("https://example.com:8443/", &fake, None).expect("pinned");
    assert_eq!(pinned.host_header, "example.com:8443");
    assert!(pinned.url.ends_with(":8443/"), "url: {}", pinned.url);
}

#[test]
fn pinning_refuses_a_url_without_a_host() {
    let fake = FakeHost::default();
    let error = pinned_target("file:///etc/passwd", &fake, None).expect_err("refused");
    assert!(error.contains("no host"), "error: {error}");
}

#[test]
fn pinning_refuses_a_private_target() {
    let fake = host(&[("127.0.0.1", "127.0.0.1")]);
    assert!(pinned_target("http://127.0.0.1:8080/", &fake, None).is_err());
}

#[test]
fn pinning_honours_the_named_host_allowlist_but_not_a_different_host() {
    let mut fake = host(&[
        ("staging.internal", "10.0.0.9"),
        ("other-internal.example", "10.0.0.9"),
    ]);
    fake.env.insert(
        PRIVATE_HOST_ALLOWLIST_ENV.to_owned(),
        "staging.internal".to_owned(),
    );

    let pinned = pinned_target("https://staging.internal/", &fake, None).expect("allowed");
    assert_eq!(pinned.host_header, "staging.internal");
    assert!(pinned.url.contains("10.0.0.9"), "url: {}", pinned.url);

    assert!(pinned_target("https://other-internal.example/", &fake, None).is_err());
}

#[test]
fn pinning_prefers_a_vetted_ipv4_answer_for_an_ipv4_only_origin() {
    let mut fake = host(&[("crawl.localhost", "::1"), ("crawl.localhost", "127.0.0.1")]);
    fake.env.insert(
        PRIVATE_HOST_ALLOWLIST_ENV.to_owned(),
        "crawl.localhost".to_owned(),
    );

    let pinned = pinned_target("https://crawl.localhost/", &fake, None).expect("pinned");
    assert!(
        pinned.url.starts_with("https://127.0.0.1/"),
        "url: {}",
        pinned.url
    );
    assert_eq!(pinned.host_header, "crawl.localhost");
    assert_eq!(pinned.sni_hostname, "crawl.localhost");
}

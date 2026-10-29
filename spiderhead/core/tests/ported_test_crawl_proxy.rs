//! Ports of the offline proxy-policy tests in the reference suite.
//!
//! Only the pure parts are ported: settings resolution, proxy route validation, DNS guards and
//! run-log redaction. Everything that opens a socket, drives httpx, the CLI, MCP or NativeScan is
//! listed in the slice report as deferred. DNS and environment come from [`FakeHost`]; nothing
//! here touches the network.

use std::net::SocketAddr;

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{self, ConfigError, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_core::net::{resolve_proxy_route, validate_url};
use spiderhead_core::runlog::{safe_arguments, Args};

/// Fake environment plus DNS answers. FakeHost ignores the port, so every answer is listed as 3128.
fn host(env: &[(&str, &str)], dns: &[(&str, &[&str])]) -> FakeHost {
    FakeHost {
        env: env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        dns: dns
            .iter()
            .map(|(name, ips)| {
                (
                    (*name).to_owned(),
                    ips.iter()
                        .map(|ip| SocketAddr::new(ip.parse().expect("literal IP"), 3128))
                        .collect(),
                )
            })
            .collect(),
        zstd: false,
    }
}

/// Dotted explicit overrides, as the reference `overrides=` argument.
fn overrides(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

/// Load settings from explicit dotted overrides only.
fn load_overrides(pairs: &[(&str, Value)], host: &FakeHost) -> Result<Value, ConfigError> {
    crawl_settings::load(
        &LoadInput {
            overrides: overrides(pairs),
            ..LoadInput::default()
        },
        host,
    )
}

#[test]
fn proxy_config_requires_explicit_safe_reference_and_never_records_auth() {
    let mut h = host(&[], &[("example.test", &["93.184.216.34"])]);
    let direct = crawl_settings::load(&LoadInput::default(), &h).expect("defaults load");
    assert_eq!(
        crawl_settings::manifest(&direct, &h).expect("manifest")["http.proxy"]["mode"],
        "direct"
    );

    h.env.insert(
        "SEOHEAD_TEST_PROXY".into(),
        "http://user:password@example.test:3128".into(),
    );
    let resolved = load_overrides(&[("http.proxy", json!("env:SEOHEAD_TEST_PROXY"))], &h)
        .expect("env reference loads");
    let route = crawl_settings::resolve_proxy(&resolved, &h)
        .expect("route resolves")
        .expect("route present");
    assert_eq!(route.identity, "http://example.test:3128");
    assert!(route.authenticated);

    let manifest = crawl_settings::manifest(&resolved, &h).expect("manifest");
    let serialized = serde_json::to_string(&manifest).expect("manifest serialises");
    assert_eq!(
        manifest["http.proxy"],
        json!({
            "mode": "proxy",
            "endpoint": "http://example.test:3128",
            "authenticated": true,
        })
    );
    assert_eq!(
        manifest["http.proxy_identity"],
        json!("http://example.test:3128")
    );
    assert_eq!(manifest["http.proxy_authenticated"], json!(true));
    assert!(!serialized.contains("password"));
    assert!(!serialized.contains("user:password"));
    assert_ne!(
        crawl_settings::fingerprint(&resolved, &h).expect("fingerprint"),
        crawl_settings::fingerprint(&direct, &h).expect("fingerprint"),
    );
    assert_eq!(
        crawl_settings::parse_setting_assignment("http.proxy=env:SEOHEAD_TEST_PROXY")
            .expect("assignment parses"),
        ("http.proxy".to_owned(), json!("env:SEOHEAD_TEST_PROXY")),
    );
    let err = load_overrides(
        &[
            ("http.proxy", json!("env:SEOHEAD_TEST_PROXY")),
            ("cache.mode", json!("live")),
        ],
        &h,
    )
    .expect_err("a proxy with live cache is refused");
    assert!(err.0.contains("cache.mode=off"), "{}", err.0);
}

#[test]
fn unsupported_proxy_is_refused_before_network() {
    let cases = [
        (
            "http://user:secret@example.test:3128",
            "inline authentication",
        ),
        ("https://example.test:3128", "only an http"),
        ("socks5h://example.test:3128", "only an http"),
        ("http://example.test", "explicit port"),
        ("http://example.test:3128/path", "path"),
        ("env:MISSING_PROXY", "missing or empty"),
    ];
    for (value, reason) in cases {
        let h = host(&[], &[("example.test", &["93.184.216.34"])]);
        let err = load_overrides(&[("http.proxy", json!(value))], &h).expect_err(value);
        assert!(err.0.contains(reason), "{value}: {}", err.0);
        assert!(!err.0.contains("secret"), "{value}: {}", err.0);
    }
}

// The decoded (user@team, p:q) pair is not asserted: ProxyRoute does not expose it yet (see slice report).
#[test]
fn proxy_auth_percent_encoding_is_decoded_and_ambiguous_usernames_rejected() {
    let mut h = host(
        &[(
            "SEOHEAD_TEST_PROXY",
            "http://user%40team:p%3Aq@example.test:3128",
        )],
        &[("example.test", &["93.184.216.34"])],
    );
    let route = resolve_proxy_route("env:SEOHEAD_TEST_PROXY", false, &h)
        .expect("encoded credentials resolve")
        .expect("route present");
    assert!(route.authenticated);

    h.env.insert(
        "SEOHEAD_TEST_PROXY".into(),
        "http://user%3Ateam:p@example.test:3128".into(),
    );
    let err = resolve_proxy_route("env:SEOHEAD_TEST_PROXY", false, &h)
        .expect_err("a colon in the username is refused");
    assert!(err.contains("username cannot contain a colon"), "{err}");
}

#[test]
fn private_proxy_opt_in_does_not_allow_private_target() {
    let h = host(
        &[],
        &[
            ("127.0.0.1", &["127.0.0.1"]),
            ("example.test", &["127.0.0.1"]),
        ],
    );
    let err = resolve_proxy_route("http://127.0.0.1:3128", false, &h)
        .expect_err("a private proxy needs the opt-in");
    assert!(err.contains("proxy_allow_private"), "{err}");

    let route = resolve_proxy_route("http://127.0.0.1:3128", true, &h)
        .expect("opt-in accepted")
        .expect("route present");
    assert_eq!(route.identity, "http://127.0.0.1:3128");

    let err = validate_url("https://example.test/private", &h, None)
        .expect_err("a target resolving to a private address is refused");
    assert!(err.contains("private"), "{err}");
}

#[test]
fn mixed_proxy_dns_answers_are_refused_before_connection() {
    let h = host(
        &[],
        &[("proxy.example.test", &["93.184.216.34", "127.0.0.1"])],
    );
    let err = resolve_proxy_route("http://proxy.example.test:3128", false, &h)
        .expect_err("a mixed DNS answer with a private address is refused");
    assert!(err.contains("private address"), "{err}");
}

#[test]
fn inline_proxy_credentials_never_reach_runlog_or_parse_errors() {
    let secret_url = "http://username:supersecret@example.test:3128";
    let args = |value: Value| -> Args { value.as_object().expect("object").clone() };

    assert_eq!(
        safe_arguments(&args(json!({"overrides": {"http.proxy": secret_url}}))),
        args(json!({"overrides": {"http.proxy": "[redacted]"}})),
    );
    assert_eq!(
        safe_arguments(&args(json!({"params": {"keywords": ["first"]}}))),
        args(json!({"params": {"keywords": ["first"]}})),
    );

    let err = crawl_settings::parse_setting_assignment(&format!("http.proxy {secret_url}"))
        .expect_err("an assignment without '=' is refused");
    assert!(!err.0.contains("supersecret"), "{}", err.0);
}

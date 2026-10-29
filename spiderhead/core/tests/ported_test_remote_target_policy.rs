//! the staging allowlist and the DNS answer rules of the remote egress policy.
//!
//! Not ported here (see the report): transport, pinned-client, browser-route, job-runner and
//! crawl-entry tests. They drive the reference httpx/Playwright mechanics, which live in crates
//! that are not ported yet.

use std::net::{IpAddr, SocketAddr};

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{load, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_core::remote_policy::RemoteEgressPolicy;

const PROJECT: &str = "project-a";
const PUBLIC_HOST: &str = "public.example.test";
const PUBLIC_URL: &str = "https://public.example.test/";

/// A fake host whose DNS answers are the given addresses on port 443.
fn host_with_dns(entries: &[(&str, &[&str])]) -> FakeHost {
    let mut host = FakeHost::default();
    for (name, addresses) in entries {
        let answers = addresses
            .iter()
            .map(|address| SocketAddr::new(address.parse::<IpAddr>().expect("ip"), 443))
            .collect();
        host.dns.insert((*name).to_owned(), answers);
    }
    host
}

/// reference `settings.load(overrides=...)` with the given dotted overrides.
fn load_config(overrides: &[(&str, Value)]) -> Value {
    let overrides: Map<String, Value> = overrides
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    load(
        &LoadInput {
            file: None,
            overrides,
            base_overrides: Map::new(),
        },
        &FakeHost::default(),
    )
    .expect("settings load")
}

/// reference `_config()`: the default settings with a bounded request budget.
fn config() -> Value {
    load_config(&[("limits.max_requests", json!(20))])
}

/// reference `RemoteEgressPolicy("project-a", ...)` with its default caps and a total of 200.
fn policy(hosts: &[&str]) -> RemoteEgressPolicy {
    RemoteEgressPolicy::new(
        PROJECT,
        hosts.iter().map(|host| (*host).to_owned()),
        1000,
        200,
        4,
        0.5,
        false,
    )
    .expect("policy")
}

#[test]
fn submission_requires_project_safe_target_and_finite_budgets() {
    let host = host_with_dns(&[(PUBLIC_HOST, &["93.184.216.34"])]);
    let policy = policy(&[]);
    policy
        .authorize_submission(PROJECT, PUBLIC_URL, &config(), &host)
        .expect("public target is accepted");

    let foreign = policy
        .authorize_submission("project-b", PUBLIC_URL, &config(), &host)
        .unwrap_err();
    assert_eq!(foreign.code, "foreign_project");

    for url in [
        "file:///etc/passwd",
        "https://user:secret@public.example.test/",
        "https://public.example.test/#fragment",
        "http://127.0.0.1/",
    ] {
        let rejected = policy
            .authorize_submission(PROJECT, url, &config(), &host)
            .unwrap_err();
        assert_eq!(rejected.code, "unsafe_target", "{url}");
        assert!(!rejected.to_string().contains("secret"));
    }

    let unbounded = policy
        .authorize_submission(PROJECT, PUBLIC_URL, &load_config(&[]), &host)
        .unwrap_err();
    assert_eq!(unbounded.code, "budget_exceeded");

    let too_fast = policy
        .authorize_submission(
            PROJECT,
            PUBLIC_URL,
            &load_config(&[
                ("limits.max_requests", json!(20)),
                ("speed.min_delay_seconds", json!(0)),
            ]),
            &host,
        )
        .unwrap_err();
    assert_eq!(too_fast.code, "budget_exceeded");

    let mut proxied = config();
    proxied["http"] = json!({"proxy": "http://proxy.example.test:8080"});
    let proxy = policy
        .authorize_submission(PROJECT, PUBLIC_URL, &proxied, &host)
        .unwrap_err();
    assert!(["invalid_config", "unsupported_proxy"].contains(&proxy.code));

    let mut remote_browser = config();
    remote_browser["rendering"]["browser"]["transport"] = json!("remote");
    let backend = policy
        .authorize_submission(PROJECT, PUBLIC_URL, &remote_browser, &host)
        .unwrap_err();
    assert!(["invalid_config", "unsupported_browser"].contains(&backend.code));
}

#[test]
fn browser_policy_applies_python_truthiness_not_types() {
    // Review 2026-10-27: reference reads `browser.get("persistent_profile")` and
    // `browser.get("backend", "local") != "local"` on raw JSON values — a truthy
    // non-boolean profile or any non-string backend must still be refused.
    let host = host_with_dns(&[(PUBLIC_HOST, &["93.184.216.34"])]);
    let policy = policy(&[]);

    for profile in [json!(1), json!("yes"), json!([true])] {
        let mut cfg = config();
        cfg["rendering"]["browser"]["persistent_profile"] = profile.clone();
        cfg["rendering"]["browser"]["persistent_profile_dir"] = json!("/tmp/profile");
        let err = policy
            .authorize_submission(PROJECT, PUBLIC_URL, &cfg, &host)
            .unwrap_err();
        assert_eq!(err.code, "unsupported_browser", "{profile}");
    }

    // `backend` is not a declared setting: the reference validate() refuses the unknown key
    // (invalid_config) before the policy reads it, so the truthiness arm stays defensive.
    for backend_value in [json!(0), json!(null), json!(["remote"])] {
        let mut cfg = config();
        cfg["rendering"]["browser"]["backend"] = backend_value.clone();
        let err = policy
            .authorize_submission(PROJECT, PUBLIC_URL, &cfg, &host)
            .unwrap_err();
        assert_eq!(err.code, "invalid_config", "{backend_value}");
    }

    // Falsy JSON values stay permitted: reference treats them like an absent flag.
    for profile in [json!(0), json!(""), json!([]), json!({})] {
        let mut cfg = config();
        cfg["rendering"]["browser"]["persistent_profile"] = profile;
        policy
            .authorize_submission(PROJECT, PUBLIC_URL, &cfg, &host)
            .expect("falsy profile is allowed");
    }
}

#[test]
fn remote_numeric_caps_require_positive_integers() {
    // Only the zero case is representable here: the Rust constructor takes u64, so the reference
    // bool and float variants cannot be written.
    for (origin, total, concurrency) in [(0, 1, 1), (1, 0, 1), (1, 1, 0)] {
        let error = RemoteEgressPolicy::new(
            PROJECT,
            Vec::<String>::new(),
            origin,
            total,
            concurrency,
            0.5,
            false,
        )
        .unwrap_err();
        assert!(error.0.contains("positive integers"), "{error}");
    }
}

#[test]
fn remote_delay_floor_rejects_nonfinite_values() {
    // The reference string and bool variants are not representable with an f64 parameter.
    for value in [f64::NAN, f64::INFINITY] {
        let error = RemoteEgressPolicy::new(PROJECT, Vec::<String>::new(), 1, 1, 1, value, false)
            .unwrap_err();
        assert!(error.0.contains("delay floor"), "{error}");
    }
}

#[test]
fn staging_allowlist_is_exact_and_never_authorizes_metadata() {
    let answers = host_with_dns(&[
        ("stage.example.test", &["10.1.2.3"]),
        ("other.example.test", &["10.1.2.3"]),
    ]);
    let policy = policy(&["STAGE.EXAMPLE.TEST."]);
    policy
        .authorize_submission(PROJECT, "https://stage.example.test/", &config(), &answers)
        .expect("exact staging host is accepted");
    assert!(policy
        .authorize_submission(PROJECT, "https://other.example.test/", &config(), &answers)
        .is_err());

    let metadata = host_with_dns(&[("stage.example.test", &["169.254.169.254"])]);
    assert!(policy
        .authorize_submission(PROJECT, "https://stage.example.test/", &config(), &metadata)
        .is_err());

    assert!(RemoteEgressPolicy::new(
        PROJECT,
        vec!["*.example.test".to_owned()],
        1,
        1,
        1,
        0.5,
        false
    )
    .is_err());
    let ip_literal =
        RemoteEgressPolicy::new(PROJECT, vec!["10.1.2.3".to_owned()], 1, 1, 1, 0.5, false)
            .unwrap_err();
    assert!(ip_literal.0.contains("DNS hostnames"), "{ip_literal}");
}

#[test]
fn private_host_allowlist_rejects_numeric_aliases() {
    // The reference integer alias 3232235777 is not representable as a Rust String; the text forms are.
    for alias in ["3232235777", "0xc0a80101", "0300.0250.1.1"] {
        let error = RemoteEgressPolicy::new(PROJECT, vec![alias.to_owned()], 1, 1, 1, 0.5, false)
            .unwrap_err();
        assert!(error.0.contains("DNS hostname"), "{alias}: {error}");
    }
}

#[test]
fn mixed_dns_answers_are_rejected_for_remote_jobs() {
    let host = host_with_dns(&[(PUBLIC_HOST, &["93.184.216.34", "10.1.2.3"])]);
    let rejected = policy(&[])
        .authorize_submission(PROJECT, PUBLIC_URL, &config(), &host)
        .unwrap_err();
    assert_eq!(rejected.code, "unsafe_target");
}

#[test]
fn allowlisted_staging_name_rejects_mixed_public_private_answers() {
    let host = host_with_dns(&[("stage.example.test", &["93.184.216.34", "10.1.2.3"])]);
    let rejected = policy(&["stage.example.test"])
        .authorize_submission(PROJECT, "https://stage.example.test/", &config(), &host)
        .unwrap_err();
    assert_eq!(rejected.code, "unsafe_target");
}

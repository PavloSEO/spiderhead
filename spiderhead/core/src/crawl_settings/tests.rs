//! Crawl-settings tests. Each `// from tests/...` marker names the reference test the case was taken from.
//! Environment and DNS come from [`FakeHost`]; nothing here touches the network or the real env.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use super::*;
use crate::host::FakeHost;
use crate::settings_data::RESULTS_AFFECTING;

fn host_with_env(pairs: &[(&str, &str)]) -> FakeHost {
    FakeHost {
        env: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        ..FakeHost::default()
    }
}

fn overrides(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// Load with explicit dotted overrides only.
fn load_with(pairs: &[(&str, Value)], host: &FakeHost) -> Result<Value, ConfigError> {
    load(
        &LoadInput {
            overrides: overrides(pairs),
            ..LoadInput::default()
        },
        host,
    )
}

fn ok(pairs: &[(&str, Value)]) -> Value {
    load_with(pairs, &FakeHost::default()).expect("settings should load")
}

fn manifest_of(config: &Value) -> BTreeMap<String, Value> {
    manifest(config, &FakeHost::default()).expect("manifest")
}

use std::collections::BTreeMap;

#[test]
fn defaults_load_and_validate() {
    let resolved = load_with(&[], &FakeHost::default()).unwrap();
    assert_eq!(resolved["robots"]["policy"], "respect");
    assert_eq!(resolved["speed"]["concurrency"], 1);
}

#[test]
fn every_setting_is_classified_as_results_affecting_or_not() {
    // Settings that only affect cost, not results. Everything else must be results-affecting.
    let cost_only = [
        "http.timeout_seconds",
        "limits.max_response_bytes",
        "output.dir",
        "output.write_pages_jsonl",
        "output.write_decisions_jsonl",
        "speed.max_delay_seconds",
        "speed.concurrency",
        "output.write_tasks",
        "rendering.artifacts.screenshots",
        "rendering.artifacts.console_errors",
        "rendering.artifacts.screenshot_mode",
        "rendering.browser.persistent_profile_dir",
    ];
    let every: BTreeSet<String> = flatten(settings_data::defaults()).keys().cloned().collect();
    let unclassified: Vec<&String> = every
        .iter()
        .filter(|p| !RESULTS_AFFECTING.contains(&p.as_str()) && !cost_only.contains(&p.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "unclassified settings: {unclassified:?}"
    );
}

#[test]
fn results_affecting_names_only_real_settings() {
    let every: BTreeSet<String> = flatten(settings_data::defaults()).keys().cloned().collect();
    for path in RESULTS_AFFECTING {
        assert!(every.contains(*path), "{path} is not a real setting");
    }
}

#[test]
fn file_overrides_defaults() {
    let host = FakeHost::default();
    let file = load(
        &LoadInput {
            file: Some(("crawl.json", r#"{"limits": {"max_urls": 42}}"#)),
            ..LoadInput::default()
        },
        &host,
    )
    .unwrap();
    assert_eq!(file["limits"]["max_urls"], 42);
}

#[test]
fn environment_overrides_the_file() {
    let host = host_with_env(&[("SEOHEAD_CRAWL_MAX_URLS", "7")]);
    let resolved = load(
        &LoadInput {
            file: Some(("crawl.json", r#"{"limits": {"max_urls": 42}}"#)),
            ..LoadInput::default()
        },
        &host,
    )
    .unwrap();
    assert_eq!(resolved["limits"]["max_urls"], 7);
}

#[test]
fn explicit_arguments_override_the_environment() {
    let host = host_with_env(&[("SEOHEAD_CRAWL_MAX_URLS", "7")]);
    let resolved = load_with(&[("limits.max_urls", json!(3))], &host).unwrap();
    assert_eq!(resolved["limits"]["max_urls"], 3);
}

#[test]
fn an_unset_override_does_not_erase_a_configured_value() {
    let host = FakeHost::default();
    let resolved = load(
        &LoadInput {
            file: Some(("crawl.json", r#"{"limits": {"max_urls": 42}}"#)),
            overrides: overrides(&[("limits.max_urls", Value::Null)]),
            ..LoadInput::default()
        },
        &host,
    )
    .unwrap();
    assert_eq!(resolved["limits"]["max_urls"], 42);
}

#[test]
fn environment_values_take_the_type_of_the_default() {
    let host = host_with_env(&[
        ("SEOHEAD_CRAWL_MIN_DELAY", "1.5"),
        ("SEOHEAD_CRAWL_MAX_DEPTH", "2"),
    ]);
    let resolved = load_with(&[], &host).unwrap();
    assert_eq!(resolved["speed"]["min_delay_seconds"], json!(1.5));
    assert_eq!(resolved["limits"]["max_depth"], 2);
}

#[test]
fn a_malformed_environment_value_is_refused_by_name() {
    let host = host_with_env(&[("SEOHEAD_CRAWL_MAX_URLS", "lots")]);
    let error = load_with(&[], &host).unwrap_err();
    assert!(error.0.contains("SEOHEAD_CRAWL_MAX_URLS"), "{error}");
}

#[test]
fn an_unknown_setting_is_refused_with_its_path() {
    let host = FakeHost::default();
    let error = load(
        &LoadInput {
            file: Some(("crawl.json", r#"{"scope": {"exclude_pattern": ["typo"]}}"#)),
            ..LoadInput::default()
        },
        &host,
    )
    .unwrap_err();
    assert!(error.0.contains("scope.exclude_pattern"), "{error}");
}

#[test]
fn free_form_headers_are_a_leaf_not_a_branch() {
    let host = FakeHost::default();
    let resolved = load(
        &LoadInput {
            file: Some((
                "crawl.json",
                r#"{"http": {"headers": {"Accept-Language": "de"}}}"#,
            )),
            ..LoadInput::default()
        },
        &host,
    )
    .unwrap();
    assert_eq!(resolved["http"]["headers"]["Accept-Language"], "de");
}

#[test]
fn generic_headers_refuse_credentials_without_echoing_their_values() {
    for name in ["Authorization", "cookie", "X-API-Key", "X-Auth-Token"] {
        let error = load_with(
            &[("http.headers", json!({ name: "dummy-inline-value" }))],
            &FakeHost::default(),
        )
        .unwrap_err();
        assert!(error.0.contains("credential_headers"), "{error}");
        assert!(!error.0.contains("dummy-inline-value"), "{error}");
    }
}

#[test]
fn invalid_values_are_refused() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("robots.policy", json!("maybe"), "robots.policy"),
        ("scope.internal", json!("everything"), "scope.internal"),
        ("limits.max_urls", json!(-1), "max_urls"),
        ("limits.max_depth", json!(-2), "max_depth"),
        (
            "limits.max_query_variants_per_path",
            json!(-1),
            "max_query_variants_per_path",
        ),
        ("speed.min_delay_seconds", json!(-1), "min_delay_seconds"),
        ("speed.concurrency", json!(0), "concurrency"),
        ("cache.mode", json!("always"), "cache.mode"),
        ("rendering.mode", json!("always"), "rendering.mode"),
        (
            "rendering.browser.viewport",
            json!("tablet"),
            "rendering.browser.viewport",
        ),
        (
            "rendering.browser.wait_until",
            json!("instant"),
            "rendering.browser.wait_until",
        ),
        (
            "rendering.browser.device_pixel_ratio",
            json!(0),
            "device_pixel_ratio",
        ),
        (
            "rendering.escalation.policy",
            json!("always"),
            "rendering.escalation.policy",
        ),
        (
            "rendering.escalation.sample_per_pattern",
            json!(0),
            "sample_per_pattern",
        ),
        (
            "rendering.browser.engine",
            json!("safari"),
            "rendering.browser.engine",
        ),
        (
            "rendering.browser.viewport_width",
            json!(800),
            "must be set together",
        ),
        (
            "rendering.browser.viewport_height",
            json!(600),
            "must be set together",
        ),
        (
            "rendering.browser.page_concurrency",
            json!(0),
            "between 1 and",
        ),
        (
            "rendering.browser.page_concurrency",
            json!(17),
            "between 1 and",
        ),
        (
            "rendering.browser.page_concurrency",
            json!(2.5),
            "must be an integer between 1 and",
        ),
        (
            "fetch.retry_server_errors",
            json!(4),
            "must be an integer between 0 and 3",
        ),
        (
            "fetch.retry_server_errors",
            json!(true),
            "must be an integer between 0 and 3",
        ),
        (
            "fetch.retry_server_errors",
            json!(2.5),
            "must be an integer between 0 and 3",
        ),
    ];
    for (path, value, message) in cases {
        let error = load_with(&[(path, value)], &FakeHost::default()).unwrap_err();
        assert!(error.0.contains(message), "{path}: {error}");
    }
}

#[test]
fn paired_viewport_and_segment_refusals() {
    let host = FakeHost::default();
    let engine = load_with(
        &[
            ("rendering.browser.engine", json!("firefox")),
            ("rendering.browser.mobile_emulation", json!(true)),
        ],
        &host,
    )
    .unwrap_err();
    assert!(engine.0.contains("cannot emulate"), "{engine}");
    let big = load_with(
        &[
            ("rendering.browser.viewport_width", json!(20000)),
            ("rendering.browser.viewport_height", json!(600)),
        ],
        &host,
    )
    .unwrap_err();
    assert!(big.0.contains("16384"), "{big}");
    let reserved = load_with(
        &[(
            "scope.segments",
            json!([{"name": "default", "prefix": "/en/"}]),
        )],
        &host,
    )
    .unwrap_err();
    assert!(reserved.0.contains("reserved"), "{reserved}");
    let dup = load_with(
        &[(
            "scope.segments",
            json!([{"name": "en", "prefix": "/en/"}, {"name": "en", "host": "en.example.com"}]),
        )],
        &host,
    )
    .unwrap_err();
    assert!(dup.0.contains("duplicate name"), "{dup}");
    let unknown = load_with(&[("scope.segments_only", json!(["fr"]))], &host).unwrap_err();
    assert!(unknown.0.contains("scope.segments_only"), "{unknown}");
}

#[test]
fn the_manifest_records_resolved_values_not_their_source() {
    let m = manifest_of(&ok(&[("limits.max_urls", json!(11))]));
    assert_eq!(m["limits.max_urls"], 11);
}

#[test]
fn two_runs_differing_in_a_results_affecting_setting_differ_in_the_manifest() {
    let first = manifest_of(&ok(&[("robots.policy", json!("respect"))]));
    let second = manifest_of(&ok(&[("robots.policy", json!("report_only"))]));
    assert_ne!(first, second);
    let differing: Vec<&String> = first.keys().filter(|k| first[*k] != second[*k]).collect();
    assert_eq!(differing, vec!["robots.policy"]);
}

#[test]
fn a_cost_only_setting_does_not_change_the_manifest() {
    let first = manifest_of(&ok(&[("output.dir", json!("/tmp/a"))]));
    let second = manifest_of(&ok(&[("output.dir", json!("/tmp/b"))]));
    assert_eq!(first, second);
}

#[test]
fn the_manifest_is_json_serialisable() {
    let value: Map<String, Value> = manifest_of(&ok(&[])).into_iter().collect();
    assert!(serde_json::to_string(&value).is_ok());
}

#[test]
fn fingerprint_is_stable_for_the_same_manifest() {
    let host = FakeHost::default();
    assert_eq!(
        fingerprint(&ok(&[]), &host).unwrap(),
        fingerprint(&ok(&[]), &host).unwrap()
    );
}

#[test]
fn fingerprint_changes_with_a_results_affecting_setting() {
    let host = FakeHost::default();
    let first = fingerprint(&ok(&[("limits.max_depth", json!(3))]), &host).unwrap();
    let second = fingerprint(&ok(&[("limits.max_depth", json!(4))]), &host).unwrap();
    assert_ne!(first, second);
}

#[test]
fn fingerprint_ignores_a_cost_only_setting() {
    let host = FakeHost::default();
    let first = fingerprint(&ok(&[("output.dir", json!("/tmp/a"))]), &host).unwrap();
    let second = fingerprint(&ok(&[("output.dir", json!("/tmp/b"))]), &host).unwrap();
    assert_eq!(first, second);
}

#[test]
fn the_effective_rate_is_derived_from_the_combination_not_one_knob() {
    assert_eq!(
        effective_request_rate(&ok(&[("speed.min_delay_seconds", json!(0.5))])),
        2.0
    );
    assert_eq!(
        effective_request_rate(&ok(&[("speed.min_delay_seconds", json!(2.0))])),
        0.5
    );
}

#[test]
fn no_delay_reports_an_unbounded_rate() {
    let rate = effective_request_rate(&ok(&[("speed.min_delay_seconds", json!(0))]));
    assert!(rate.is_infinite());
}

#[test]
fn a_rate_above_two_requests_per_second_warns_and_two_does_not() {
    let two = ok(&[("speed.min_delay_seconds", json!(0.5))]);
    let above = ok(&[("speed.min_delay_seconds", json!(0.4))]);
    assert!(rate_warnings(&two).is_empty());
    assert_eq!(rate_warnings(&above).len(), 1);
}

#[test]
fn rate_fields_report_unbounded_with_no_delay() {
    let fields = rate_fields(&ok(&[("speed.min_delay_seconds", json!(0))]));
    assert_eq!(fields["effective_max_requests_per_second"], "unbounded");
    // An unbounded rate is above the 2 req/s threshold, so it carries one warning.
    assert_eq!(fields["warnings"].as_array().map(Vec::len), Some(1));
    assert_eq!(fields["load_estimate"]["request_rate_state"], "unbounded");
}

#[test]
fn credential_headers_apply_only_to_their_own_host() {
    let host = host_with_env(&[("API_TOKEN", "secret-value")]);
    let entries =
        vec![json!({"host": "a.example.test", "headers": {"Authorization": "env:API_TOKEN"}})];
    let own = resolve_credential_headers(&entries, "A.example.test", &host);
    assert_eq!(own["Authorization"], "secret-value");
    assert!(resolve_credential_headers(&entries, "b.example.test", &host).is_empty());
}

#[test]
fn an_unbound_credential_entry_is_refused() {
    let host = host_with_env(&[("API_TOKEN", "x")]);
    let error = load_with(
        &[
            ("http.credentials_acknowledged", json!(true)),
            (
                "http.credential_headers",
                json!([{"headers": {"Authorization": "env:API_TOKEN"}}]),
            ),
        ],
        &host,
    )
    .unwrap_err();
    assert!(error.0.contains("host binding"), "{error}");
}

#[test]
fn credentials_without_acknowledgement_are_refused() {
    let host = host_with_env(&[("API_TOKEN", "x")]);
    let error = load_with(
        &[(
            "http.credential_headers",
            json!([{"host": "a.example.test", "headers": {"Authorization": "env:API_TOKEN"}}]),
        )],
        &host,
    )
    .unwrap_err();
    assert!(error.0.contains("credentials_acknowledged"), "{error}");
}

#[test]
fn an_inline_credential_value_is_refused() {
    let host = host_with_env(&[]);
    let error = load_with(
        &[
            ("http.credentials_acknowledged", json!(true)),
            (
                "http.credential_headers",
                json!([{"host": "a.example.test", "headers": {"Authorization": "Bearer abc"}}]),
            ),
        ],
        &host,
    )
    .unwrap_err();
    assert!(error.0.contains("env:VAR_NAME"), "{error}");
    assert!(!error.0.contains("Bearer abc"), "{error}");
}

#[test]
fn configuring_credentials_adds_the_default_destructive_path_exclusions() {
    let host = host_with_env(&[("API_TOKEN", "x")]);
    let resolved = load_with(
        &[
            ("http.credentials_acknowledged", json!(true)),
            (
                "http.credential_headers",
                json!([{"host": "a.example.test", "headers": {"Authorization": "env:API_TOKEN"}}]),
            ),
        ],
        &host,
    )
    .unwrap();
    let patterns: Vec<&str> = resolved["scope"]["exclude_patterns"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for pattern in DESTRUCTIVE_PATH_PATTERNS {
        assert!(patterns.contains(pattern), "{pattern} missing");
    }
}

#[test]
fn credential_values_are_redacted_from_the_manifest() {
    let host = host_with_env(&[("API_TOKEN", "super-secret")]);
    let resolved = load_with(
        &[
            ("http.credentials_acknowledged", json!(true)),
            (
                "http.credential_headers",
                json!([{"host": "a.example.test", "headers": {"Authorization": "env:API_TOKEN"}}]),
            ),
        ],
        &host,
    )
    .unwrap();
    let text = serde_json::to_string(&manifest(&resolved, &host).unwrap()).unwrap();
    assert!(!text.contains("super-secret"));
    assert!(text.contains("REDACTED"));
}

#[test]
fn manifest_redacts_credentials_from_a_legacy_generic_header_mapping() {
    let redacted = redact_sensitive_headers(&json!({"Cookie": "s=1", "Accept": "text/html"}));
    assert_eq!(redacted["Cookie"], "REDACTED");
    assert_eq!(redacted["Accept"], "text/html");
}

#[test]
fn zstd_requires_a_bounded_decoder() {
    let error = load_with(&[("http.zstd", json!(true))], &FakeHost::default()).unwrap_err();
    assert!(error.0.contains("http.zstd requires"), "{error}");
    let host = FakeHost {
        zstd: true,
        ..FakeHost::default()
    };
    let resolved = load_with(&[("http.zstd", json!(true))], &host).unwrap();
    let headers = request_headers(&resolved).unwrap();
    assert!(headers["Accept-Encoding"]
        .as_str()
        .unwrap()
        .contains("zstd"));
}

#[test]
fn a_persistent_profile_without_a_directory_is_refused() {
    let error = load_with(
        &[("rendering.browser.persistent_profile", json!(true))],
        &FakeHost::default(),
    )
    .unwrap_err();
    assert!(error.0.contains("persistent_profile_dir"), "{error}");
}

#[test]
fn a_persistent_profile_with_a_directory_loads() {
    let resolved = load_with(
        &[
            ("rendering.browser.persistent_profile", json!(true)),
            (
                "rendering.browser.persistent_profile_dir",
                json!("/var/tmp/profile"),
            ),
        ],
        &FakeHost::default(),
    )
    .unwrap();
    assert_eq!(resolved["rendering"]["browser"]["persistent_profile"], true);
}

#[test]
fn a_segment_config_loads_and_validates() {
    let resolved = load_with(
        &[(
            "scope.segments",
            json!([{"name": "en", "prefix": "/en/"}, {"name": "blog", "host": "blog.example.test"}]),
        )],
        &FakeHost::default(),
    )
    .unwrap();
    assert_eq!(resolved["scope"]["segments"].as_array().unwrap().len(), 2);
}

#[test]
fn cache_invalidate_is_refused_with_replay_mode() {
    let error = load_with(
        &[
            ("cache.mode", json!("replay")),
            ("cache.invalidate", json!(true)),
        ],
        &FakeHost::default(),
    )
    .unwrap_err();
    assert!(
        error
            .0
            .contains("cannot be combined with cache.mode='replay'"),
        "{error}"
    );
}

#[test]
fn http_auth_refuses_non_env_credentials() {
    let host = host_with_env(&[("U", "user")]);
    let error = load_with(
        &[
            ("http.auth_acknowledged", json!(true)),
            (
                "http.auth",
                json!([{"host": "a.example.test", "scheme": "basic", "username_env": "env:U", "password_env": "plain"}]),
            ),
        ],
        &host,
    )
    .unwrap_err();
    assert!(error.0.contains("password_env"), "{error}");
}

// Behaviour of parse_setting_assignment.
#[test]
fn set_assignment_refuses_unknown_path_and_bad_value() {
    let typo = parse_setting_assignment("limits.concurrency=2").unwrap_err();
    assert!(typo.0.contains("did you mean speed.concurrency?"), "{typo}");
    let (path, value) = parse_setting_assignment("speed.concurrency=3").unwrap();
    assert_eq!((path.as_str(), value), ("speed.concurrency", json!(3)));
    assert!(parse_setting_assignment("limits.max_urls=many").is_err());
}

#[test]
fn flatten_stops_at_free_form_headers() {
    let flat = flatten(&json!({"http": {"headers": {"A": "1"}, "user_agent": "x"}}));
    assert!(flat.contains_key("http.headers"));
    assert!(!flat.contains_key("http.headers.A"));
}

// Behaviour of checked_url_budget: a request above the ceiling is refused, never clamped.
#[test]
fn checked_url_budget_refuses_above_ceiling_instead_of_clamping() {
    assert_eq!(checked_url_budget(10, false).unwrap(), 10);
    assert!(checked_url_budget(MAX_URLS_CEILING + 1, false).is_err());
    assert!(checked_url_budget(0, true).is_err());
}

// Behaviour of bind_budgets_to_page_budget: unset budgets follow the page budget.
#[test]
fn page_budget_binds_unset_request_and_time_budgets() {
    let mut settings = ok(&[("limits.max_urls", json!(100))]);
    bind_budgets_to_page_budget(&mut settings, &BTreeSet::new());
    assert_eq!(settings["limits"]["max_requests"], 300);
    assert_eq!(settings["limits"]["max_crawl_seconds"], 200);
}

// Behaviour of delay_for_request_rate and the inverse effective rate.
#[test]
fn delay_is_the_inverse_of_rate() {
    assert_eq!(delay_for_request_rate(2.0).unwrap(), 0.5);
    assert!(delay_for_request_rate(0.0).is_err());
}

/// Values JSON cannot hold are tagged: `{"$nonfinite": ...}` for `inf`/`nan` and
/// `{"$bigint": ...}` for integers beyond 64 bits. A `serde_json` number cannot hold either, so
/// a tagged value is rejected.
fn is_unrepresentable(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key("$nonfinite")
                || map.contains_key("$bigint")
                || map.values().any(is_unrepresentable)
        }
        Value::Array(items) => items.iter().any(is_unrepresentable),
        _ => false,
    }
}

/// Table of expected fingerprints: the SHA-256 of the manifest JSON with sorted keys and non-ASCII kept.
#[test]
fn fingerprint_matches_oracle_differential_table() {
    let rows: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/fingerprint_oracle.json"))
            .expect("fixture parses");
    let rows = rows.as_array().expect("array");
    assert!(rows.len() >= 150);
    let host = FakeHost::default();
    let mut bad = Vec::new();
    for row in rows {
        let config = merge(crate::settings_data::defaults(), &row["overlay"]);
        let got = fingerprint(&config, &host);
        if got.as_deref().ok() != row["fp"].as_str() {
            bad.push(format!(
                "{}: rust {got:?}, oracle {}",
                row["overlay"], row["fp"]
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Differential table for `coerce` and `parse_setting_assignment` (float and int parsing, bool and
/// list splitting, JSON objects: each case yields the expected value or rejection.
#[test]
fn coerce_and_assignment_match_oracle_differential_table() {
    let rows: Value = serde_json::from_str(include_str!("../../tests/fixtures/coerce_oracle.json"))
        .expect("fixture parses");
    let rows = rows.as_array().expect("array");
    assert!(rows.len() >= 100);
    let mut bad = Vec::new();
    for row in rows {
        let (label, got): (String, Result<Value, String>) = if row["fn"] == "coerce" {
            let (path, raw) = (
                row["path"].as_str().unwrap_or(""),
                row["raw"].as_str().unwrap_or(""),
            );
            (format!("coerce {path} {raw:?}"), coerce(path, raw))
        } else {
            let text = row["text"].as_str().unwrap_or("");
            (
                format!("assign {text:?}"),
                parse_setting_assignment(text)
                    .map(|(p, v)| json!([p, v]))
                    .map_err(|e| e.to_string()),
            )
        };
        let same = match row.get("ok") {
            Some(want) if is_unrepresentable(want) => got.is_err(),
            Some(want) => got.as_ref().ok() == Some(want),
            None => got.is_err(),
        };
        if !same {
            bad.push(format!("{label}: rust {got:?}, oracle {row}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

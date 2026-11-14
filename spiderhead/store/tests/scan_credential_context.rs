//! Ports of the reference suite: a credential resume context never records
//! secrets, profile paths, or environment names. Environment-touching tests share a lock because
//! the process environment is global; each test names its reference source.

use std::sync::Mutex;

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{load, validate, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_store::credential_context::{
    credential_verifier, redact_config, validate_recorded_credentials,
};

const SCAN_UUID: &str = "12345678-1234-5678-1234-567812345678";
const SECRET_VAR: &str = "SH_PORT_TEST_SECRET";

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn loaded(overrides: &[(&str, Value)]) -> Map<String, Value> {
    let input = LoadInput {
        overrides: overrides
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
        ..LoadInput::default()
    };
    load(&input, &FakeHost::default())
        .expect("settings load")
        .as_object()
        .cloned()
        .expect("object")
}

/// reference `_settings()`: defaults with one explicit credential header and a persistent profile.
fn settings() -> Map<String, Value> {
    let mut s = loaded(&[("speed.min_delay_seconds", json!(0))]);
    s["http"]["credential_headers"] = json!([
        {"host": "Example.test", "headers": {"Authorization": format!("env:{SECRET_VAR}")}}
    ]);
    s["http"]["credentials_acknowledged"] = json!(true);
    s["rendering"]["browser"]["persistent_profile"] = json!(true);
    s["rendering"]["browser"]["persistent_profile_dir"] = json!("/private/profile");
    s
}

fn set_env(value: Option<&str>) {
    match value {
        // Edition 2021: `set_var` is safe; ENV_LOCK serialises the tests that use it.
        Some(v) => std::env::set_var(SECRET_VAR, v),
        None => std::env::remove_var(SECRET_VAR),
    }
}

#[test]
fn redaction_is_deep_and_never_leaks_environment_or_profile_path() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_env(Some("actual secret"));
    let live = settings();
    let recorded = redact_config(&live).expect("redact");
    let recorded_repr = Value::Object(recorded.clone()).to_string();
    assert_eq!(
        recorded["http"]["credential_headers"][0]["headers"],
        json!({"Authorization": "REDACTED"})
    );
    assert_eq!(
        recorded["rendering"]["browser"]["persistent_profile_dir"],
        "REDACTED"
    );
    assert!(!recorded_repr.contains(SECRET_VAR));
    assert!(!recorded_repr.contains("actual secret"));
    assert!(!recorded_repr.contains("private/profile"));
    // The live settings are untouched.
    assert_eq!(
        live["http"]["credential_headers"][0]["headers"]["Authorization"],
        format!("env:{SECRET_VAR}")
    );
    set_env(None);
}

#[test]
fn redaction_defensively_hides_credentials_from_a_legacy_generic_header_mapping() {
    let mut live = settings();
    live["http"]["headers"] = json!({"Cookie": "dummy-inline-value", "Accept-Language": "de"});
    let recorded = redact_config(&live).expect("redact");
    assert_eq!(
        recorded["http"]["headers"],
        json!({"Cookie": "REDACTED", "Accept-Language": "de"})
    );
    assert_eq!(live["http"]["headers"]["Cookie"], "dummy-inline-value");
}

#[test]
fn recorded_snapshot_validates_without_environment_and_yields_safe_settings_copy() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_env(None);
    let recorded = redact_config(&settings()).expect("redact");
    let safe = validate_recorded_credentials(&recorded).expect("validate recorded");
    validate(&Value::Object(safe.clone()), &FakeHost::default()).expect("safe copy is valid");
    assert_eq!(safe["http"]["credential_headers"], json!([]));
    assert_eq!(
        safe["rendering"]["browser"]["persistent_profile"],
        json!(false)
    );
    assert_eq!(
        safe["rendering"]["browser"]["persistent_profile_dir"],
        json!("")
    );
}

#[test]
fn verifier_is_stable_for_same_live_context_and_changes_with_secret_or_host() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_env(Some("one"));
    let mut current = settings();
    let first = credential_verifier(&current, SCAN_UUID).expect("verifier");
    assert_eq!(
        first,
        credential_verifier(&current.clone(), SCAN_UUID).expect("verifier")
    );
    set_env(Some("two"));
    assert_ne!(
        first,
        credential_verifier(&current, SCAN_UUID).expect("verifier")
    );
    set_env(Some("one"));
    current["http"]["credential_headers"][0]["host"] = json!("other.test");
    assert_ne!(
        first,
        credential_verifier(&current, SCAN_UUID).expect("verifier")
    );
    set_env(None);
}

#[test]
fn verifier_is_none_without_explicit_headers_and_rejects_unredacted_snapshot() {
    let plain = loaded(&[("speed.min_delay_seconds", json!(0))]);
    assert_eq!(
        credential_verifier(&plain, SCAN_UUID).expect("verifier"),
        None
    );

    let mut recorded = redact_config(&settings()).expect("redact");
    recorded["http"]["credential_headers"][0]["headers"]["Authorization"] = json!("env:LEAK");
    let err = validate_recorded_credentials(&recorded).expect_err("unredacted snapshot refused");
    assert!(err.to_string().contains("redacted"), "{err}");
}

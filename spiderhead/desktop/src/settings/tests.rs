//! Settings-store and sources-catalogue tests. Each test cites the reference
//! desktop test it ports (`desktop/tests/test_settings_*the reference suite,
//! the reference suite, the reference suite). Qt-bound tests (widget
//! states, signal plumbing, dialog geometry) have no Rust counterpart — they
//! are listed in the slice report instead.

use std::collections::BTreeMap;

use serde_json::json;
use spiderhead_core::host::FakeHost;

use super::*;

fn store() -> AppSettings {
    AppSettings::in_memory()
}

#[test]
fn agent_defaults_match_sheet() {
    let s = store();
    assert!(!s.get_bool("agent.run_scans"));
    assert!(s.get_bool("agent.change_tasks"));
    assert!(s.get_bool("agent.show_actions"));
    assert_eq!(s.get_str("agent.projects"), "");
}

#[test]
fn project_access_is_saved() {
    let mut s = store();
    assert!(s.set("agent.projects", json!("a.test\nb.test")).unwrap());
    assert!(s.is_set("agent.projects"));
    assert_eq!(s.get_str("agent.projects"), "a.test\nb.test");
}

#[test]
fn core_defaults() {
    let s = store();
    assert_eq!(s.get_str("core.poll"), "0.5");
    assert_eq!(s.get_str("core.log_level"), "INFO");
}

#[test]
fn schema_rejects_unknown_choices() {
    let mut s = store();
    assert!(s.set("core.poll", json!("3")).is_err());
    assert!(s.set("core.log_level", json!("TRACE")).is_err());
    assert_eq!(s.get_str("core.poll"), "0.5");
}

#[test]
fn controls_write_to_store() {
    let mut s = store();
    s.set("core.log_level", json!("DEBUG")).unwrap();
    assert_eq!(s.get_str("core.log_level"), "DEBUG");
}

#[test]
fn store_validates_and_stores_nothing_on_error() {
    let mut s = store();
    assert!(s.set("keys.palette", json!("Ctrl+Alt+P")).is_ok());
    assert_eq!(s.get_str("keys.palette"), "Ctrl+Alt+P");
    // No `choices` on keys.*: any chord text is accepted; unknown keys fail.
    assert!(s.set("keys.unknown", json!("Ctrl+Q")).is_err());
    assert!(!s.is_set("keys.unknown"));
}

#[test]
fn shortcuts_registry_is_consistent() {
    let mut seen = std::collections::BTreeSet::new();
    for &(action, chord) in SHORTCUTS {
        assert!(seen.insert(action), "duplicate action {action}");
        assert!(!chord.is_empty(), "empty chord for {action}");
        assert!(definition(&format!("keys.{action}")).is_some());
    }
}

#[test]
fn export_import_round_trip() {
    let mut s = store();
    s.set("keys.palette", json!("Ctrl+Shift+P")).unwrap();
    s.set("view.zoom", json!(125)).unwrap();
    let doc = s.export();
    let mut other = store();
    let (applied, rejected) = other.import(&doc);
    // The export carries every declared key's effective value.
    assert_eq!(applied, schema().len());
    assert!(rejected.is_empty());
    assert_eq!(other.get_str("keys.palette"), "Ctrl+Shift+P");
    assert_eq!(other.get("view.zoom"), json!(125));
    // A file that is not an export document imports nothing.
    let (applied, rejected) = other.import(&json!([]));
    assert_eq!(applied, 0);
    assert_eq!(rejected, vec!["file".to_string()]);
}

#[test]
fn conflicting_chords_are_detectable() {
    let mut s = store();
    s.set("keys.new_scan", json!("Ctrl+K")).unwrap();
    let chords: Vec<String> = SHORTCUTS
        .iter()
        .map(|(a, _)| s.get_str(&format!("keys.{a}")))
        .collect();
    assert_eq!(chords.iter().filter(|c| *c == "Ctrl+K").count(), 2);
}

#[test]
fn mcp_defaults() {
    let s = store();
    assert!(s.get_bool("mcp.enabled"));
    assert_eq!(s.get_str("mcp.profile"), "full");
    assert!(s.get_bool("mcp.journal"));
}

#[test]
fn mcp_profile_validation() {
    let mut s = store();
    for ok in ["full", "audit", "quick-check", "router", "infra"] {
        assert!(s.set("mcp.profile", json!(ok)).is_ok(), "profile {ok}");
    }
    assert!(s.set("mcp.profile", json!("none")).is_err());
}

#[test]
fn mcp_switch_stores() {
    let mut s = store();
    s.set("mcp.enabled", json!(false)).unwrap();
    assert!(!s.get_bool("mcp.enabled"));
    assert!(s.is_set("mcp.enabled"));
}

#[test]
fn notify_defaults_match_sheet() {
    let s = store();
    let expect: BTreeMap<&str, (bool, bool, bool)> =
        NOTIFY_EVENTS.iter().map(|(e, d)| (*e, *d)).collect();
    for (event, (w, sys, z)) in expect {
        assert_eq!(s.get_bool(&format!("notify.{event}_w")), w, "{event} w");
        assert_eq!(s.get_bool(&format!("notify.{event}_s")), sys, "{event} s");
        assert_eq!(s.get_bool(&format!("notify.{event}_z")), z, "{event} z");
    }
    assert!(s.get_bool("notify.quiet"));
    assert_eq!(s.get_str("notify.quiet_from"), "20:00");
    assert_eq!(s.get_str("notify.quiet_to"), "08:00");
}

// test_store_rejects_bad_time_directly
#[test]
fn quiet_hours_validation() {
    let mut s = store();
    assert!(s.set("notify.quiet_to", json!("8:30")).is_ok());
    assert_eq!(s.get_str("notify.quiet_to"), "08:30");
    for bad in ["8.30", "24:00", "9:60", "20:", "abc", ""] {
        assert!(
            s.set("notify.quiet_from", json!(bad)).is_err(),
            "bad time {bad:?}"
        );
    }
    assert_eq!(s.get_str("notify.quiet_from"), "20:00");
}

#[test]
fn notify_matrix_writes_back() {
    let mut s = store();
    s.set("notify.done_z", json!(true)).unwrap();
    s.set("notify.err_w", json!(false)).unwrap();
    assert!(s.get_bool("notify.done_z"));
    assert!(!s.get_bool("notify.err_w"));
}

#[test]
fn scan_defaults() {
    let s = store();
    assert_eq!(s.get("scan.rate"), json!(2.0));
    assert_eq!(s.get("scan.url_limit"), json!(1500));
    assert_eq!(s.get("scan.depth"), json!(10));
    assert!(s.get_bool("scan.robots"));
    assert!(s.get_bool("scan.save_html"));
    assert_eq!(s.get("scan.parallel"), json!(3));
    assert_eq!(s.get_str("scan.default_profile"), "quick");
}

#[test]
fn scan_limits_are_enforced() {
    let mut s = store();
    assert!(s.set("scan.rate", json!(11.0)).is_err());
    assert!(s.set("scan.url_limit", json!(0)).is_err());
    assert!(s.set("scan.depth", json!(101)).is_err());
    assert!(s.set("scan.parallel", json!(5)).is_err());
    assert!(s.set("scan.parallel", json!("4")).is_ok());
    // The Russian schema message survives for the UI to show.
    let err = s.set("scan.rate", json!("fast")).unwrap_err();
    assert!(err.contains("10"));
}

#[test]
fn scan_defaults_map_onto_crawl_settings() {
    let mut s = store();
    s.set("scan.url_limit", json!(250)).unwrap();
    s.set("scan.robots", json!(false)).unwrap();
    let crawl = s.crawl_defaults();
    assert_eq!(crawl.get("limits.max_urls"), Some(&json!(250)));
    assert_eq!(crawl.get("limits.max_depth"), Some(&json!(10)));
    assert_eq!(crawl.get("robots.policy"), Some(&json!("ignore")));
    assert_eq!(crawl.get("speed.min_delay_seconds"), Some(&json!(0.5)));
    assert_eq!(
        crawl.get("storage.body_mode"),
        Some(&json!("captured_entity_bytes"))
    );
    // App-level concurrency is not a crawl setting.
    assert!(crawl.get("scan.parallel").is_none());
    assert!(crawl.get("parallel").is_none());
}

#[test]
fn shell_defaults() {
    let s = store();
    assert_eq!(s.get_str("shell.mode"), "crawler");
    assert!(!s.get_bool("shell.onboarding_done"));
}

#[test]
fn every_section_declares_its_schema() {
    let prefixes = [
        "shell.", "general.", "view.", "scan.", "core.", "mcp.", "agent.", "notify.", "keys.",
        "data.", "updates.",
    ];
    let keys: Vec<&str> = schema().iter().map(|s| s.key.as_str()).collect();
    for prefix in prefixes {
        assert!(
            keys.iter().any(|k| k.starts_with(prefix)),
            "no settings under {prefix}"
        );
    }
}

#[test]
fn no_duplicate_keys() {
    let mut seen = std::collections::BTreeSet::new();
    for s in schema() {
        assert!(seen.insert(s.key.as_str()), "duplicate {}", s.key);
    }
}

// reference settings_store: set of an invalid value leaves the store untouched.
#[test]
fn invalid_set_stores_nothing() {
    let mut s = store();
    assert!(s.set("view.zoom", json!(200)).is_err());
    assert!(!s.is_set("view.zoom"));
    assert_eq!(s.get("view.zoom"), json!(100));
    assert!(s.set("no.such", json!(1)).is_err());
}

// reference AppSettings.get: a corrupt stored value falls back to the default.
#[test]
fn corrupt_stored_value_falls_back_to_default() {
    let mut s = store();
    s.values.insert("view.zoom".into(), json!("huge"));
    assert_eq!(s.get("view.zoom"), json!(100));
}

// reference AppSettings.set: re-storing the same value is a no-op.
#[test]
fn same_value_is_not_a_change() {
    let mut s = store();
    assert!(s.set("view.zoom", json!(110)).unwrap());
    assert!(!s.set("view.zoom", json!(110)).unwrap());
}

// reference AppSettings.reset(prefix).
#[test]
fn reset_clears_only_the_prefix() {
    let mut s = store();
    s.set("view.zoom", json!(110)).unwrap();
    s.set("view.theme", json!("dark")).unwrap();
    s.set("scan.depth", json!(20)).unwrap();
    assert_eq!(s.reset("view."), 2);
    assert_eq!(s.get("view.zoom"), json!(100));
    assert_eq!(s.get("scan.depth"), json!(20));
    assert_eq!(s.reset("view."), 0);
}

// reference AppSettings: persisted file round-trips through load/save.
#[test]
fn load_save_round_trip() {
    let dir = std::env::temp_dir().join(format!("sh-settings-{}", std::process::id()));
    let path = dir.join("desktop-settings.json");
    let mut s = AppSettings::load(path.clone());
    s.set("view.theme", json!("dark")).unwrap();
    s.set("general.recent_count", json!(7)).unwrap();
    s.save().unwrap();
    let loaded = AppSettings::load(path.clone());
    assert_eq!(loaded.get_str("view.theme"), "dark");
    assert_eq!(loaded.get("general.recent_count"), json!(7));
    let _ = std::fs::remove_dir_all(&dir);
}

// Corrupt or foreign content in the file is ignored, never crashes.
#[test]
fn corrupt_file_means_defaults() {
    let dir = std::env::temp_dir().join(format!("sh-settings-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("desktop-settings.json");
    std::fs::write(&path, "{ not json").unwrap();
    let s = AppSettings::load(path.clone());
    assert_eq!(s.get_str("view.theme"), "light");
    std::fs::write(
        &path,
        r#"{"settings":{"view.theme":"dark","nope.unknown":1,"view.zoom":999}}"#,
    )
    .unwrap();
    let s = AppSettings::load(path);
    assert_eq!(s.get_str("view.theme"), "dark");
    assert!(!s.is_set("nope.unknown"));
    // 999 was stored raw; the read still clamps to the valid default.
    assert_eq!(s.get("view.zoom"), json!(100));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn describe_reports_value_default_is_set() {
    let mut s = store();
    s.set("view.zoom", json!(120)).unwrap();
    let d = s.describe();
    let zoom = &d["view.zoom"];
    assert_eq!(zoom["value"], json!(120));
    assert_eq!(zoom["default"], json!(100));
    assert_eq!(zoom["is_set"], json!(true));
    assert_eq!(d["scan.depth"]["is_set"], json!(false));
}

#[test]
fn sources_list_never_returns_values() {
    let host = FakeHost::default();
    let root = std::env::temp_dir().join(format!("sh-src-{}", std::process::id()));
    std::fs::create_dir_all(root.join("arsenkin")).unwrap();
    std::fs::write(root.join("arsenkin/token"), "real-token-value").unwrap();
    let list = sources_list(&host, &root);
    let text = list.to_string();
    assert!(!text.contains("real-token-value"));
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), providers().len());
    let arsenkin = arr.iter().find(|p| p["id"] == "arsenkin").unwrap();
    assert_eq!(arsenkin["components"]["token"], json!(true));
    assert_eq!(arsenkin["state"], "configured_unverified");
    let wayback = arr.iter().find(|p| p["id"] == "wayback").unwrap();
    assert_eq!(wayback["state"], "not_required");
    let dataforseo = arr.iter().find(|p| p["id"] == "dataforseo").unwrap();
    assert_eq!(dataforseo["state"], "not_configured");
    let _ = std::fs::remove_dir_all(&root);
}

// sources.credential writes through the credential store, never echoing back.
#[test]
fn credential_set_and_revoke() {
    let dir = std::env::temp_dir().join(format!("sh-cred-{}", std::process::id()));
    let out = credential_action(&dir, "arsenkin", "token", "set", "typed-value").unwrap();
    assert_eq!(out["set"], json!(true));
    // The store holds it, the reply never does.
    assert!(!out.to_string().contains("typed-value"));
    assert!(dir.join("arsenkin/token").is_file());
    let out = credential_action(&dir, "arsenkin", "token", "revoke", "").unwrap();
    assert_eq!(out["set"], json!(false));
    assert!(!dir.join("arsenkin/token").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// sources.credential refuses unknown provider/component/action.
#[test]
fn credential_action_rejects_unknowns() {
    let dir = std::env::temp_dir();
    assert!(credential_action(&dir, "nope", "token", "set", "x").is_err());
    assert!(credential_action(&dir, "arsenkin", "nope", "set", "x").is_err());
    assert!(credential_action(&dir, "arsenkin", "token", "peek", "x").is_err());
    // A public provider has no components at all.
    assert!(credential_action(&dir, "wayback", "token", "set", "x").is_err());
}

// Multi-component providers need every component set for configured state.
#[test]
fn partial_credentials_read_as_invalid() {
    let host = FakeHost::default();
    let root = std::env::temp_dir().join(format!("sh-src-p-{}", std::process::id()));
    std::fs::create_dir_all(root.join("dataforseo")).unwrap();
    std::fs::write(root.join("dataforseo/login"), "l").unwrap();
    let list = sources_list(&host, &root);
    let dfs = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "dataforseo")
        .unwrap();
    assert_eq!(dfs["state"], "invalid");
    assert_eq!(dfs["components"]["login"], json!(true));
    assert_eq!(dfs["components"]["password"], json!(false));
    let _ = std::fs::remove_dir_all(&root);
}

//! Tests of the schema layer: create/open/inspect, the config header and light open.
//!
//! Scans are built with `create_file` (the part of `create` that precedes the reopen for
//! writing), with [`PassThrough`](super::oracle_tests::PassThrough) standing in for the
//! owners that are not implemented yet.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_core::settings_data;

use super::oracle_tests::passthrough;
use super::*;
use crate::scan_artifact::{open_scan, open_scan_mode};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-w2s1-schema-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// The resolved config defaults with the delay removed.
fn config() -> Map<String, Value> {
    let mut config = settings_data::defaults().clone();
    config["speed"]["min_delay_seconds"] = json!(0);
    config.as_object().cloned().expect("object")
}

fn versions() -> Map<String, Value> {
    ["python", "sqlite", "httpx", "lxml", "beautifulsoup4"]
        .iter()
        .map(|k| ((*k).to_owned(), json!("test")))
        .collect()
}

fn create_with(
    path: &Path,
    config: &Map<String, Value>,
    fingerprint: Option<&str>,
) -> Result<PathBuf, StoreError> {
    NativeScanSchemaMixin::create_file(
        path,
        "https://example.test/",
        config,
        fingerprint,
        "3.0.0",
        &"a".repeat(40),
        &versions(),
        &[],
        None,
        &[],
        "scan.v1",
    )
}

fn create(dir: &Path, name: &str) -> PathBuf {
    passthrough(|| create_with(&dir.join(name), &config(), None)).expect("create")
}

fn mutate(path: &Path, statements: &str) {
    let con = Connection::open(path).expect("open rw");
    con.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
        .expect("pragma");
    con.execute_batch(statements).expect("mutation");
}

#[track_caller]
fn refused<T>(result: Result<T, StoreError>, pattern: &str) {
    match result {
        Ok(_) => panic!("expected a refusal matching {pattern:?}"),
        Err(error) => {
            let text = error.to_string();
            assert!(
                Regex::new(pattern).expect("pattern").is_match(&text),
                "{text:?} !~ {pattern:?}"
            );
        }
    }
}

#[test]
fn create_refuses_a_non_resolved_config() {
    let dir = scratch("unresolved");
    let partial =
        json!({"speed": {"concurrency": 1}, "limits": {"max_query_variants_per_path": 1}});
    let target = dir.join("invalid.sqlite");
    refused(
        passthrough(|| create_with(&target, partial.as_object().unwrap(), None)),
        "config",
    );
    assert!(!target.exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn create_requires_non_result_config_fields_too() {
    let dir = scratch("nonresult");
    let mut incomplete = config();
    incomplete.remove("output");
    let target = dir.join("invalid.sqlite");
    refused(
        passthrough(|| create_with(&target, &incomplete, None)),
        "complete resolved",
    );
    assert!(!target.exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn new_config_fields_round_trip_outside_the_frozen_header() {
    let dir = scratch("extension");
    let mut settings = config();
    settings["scope"]["normalize_tracking_parameters"] = json!(false);
    settings["rendering"]["artifacts"]["screenshot_mode"] = json!("first_screen");
    let path = passthrough(|| create_with(&dir.join("ext.sqlite"), &settings, None)).unwrap();
    let con = passthrough(|| open_scan(&path, false, None)).unwrap();
    let (header, fp): (String, String) = con
        .query_row(
            "SELECT config_json, config_fingerprint FROM scan",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&header).unwrap();
    assert!(stored["scope"]
        .get("normalize_tracking_parameters")
        .is_none());
    assert!(stored["http"].get("auth").is_none());
    assert!(stored["rendering"].get("actions").is_none());
    assert!(stored["rendering"]["artifacts"]
        .get("screenshot_mode")
        .is_none());
    assert_eq!(fp, crawl_fingerprint(stored.as_object().unwrap()).unwrap());
    let effective = passthrough(|| read_effective_config(&con)).unwrap();
    assert_eq!(
        effective["scope"]["normalize_tracking_parameters"],
        json!(false)
    );
    assert_eq!(
        effective["rendering"]["artifacts"]["screenshot_mode"],
        json!("first_screen")
    );
    let payload: String = con
        .query_row(
            "SELECT payload_json FROM context_items WHERE kind='native_config_extension' AND item_key='run'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["schema_version"], "native_config_extension.v1");
    assert_eq!(
        payload["effective_fingerprint"].as_str().unwrap(),
        effective_config_fingerprint(&effective)
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn legacy_config_defaults_keep_tracking_parameters_and_refuse_semantic_resume() {
    let dir = scratch("legacy-header");
    let mut settings = config();
    settings["scope"]["normalize_tracking_parameters"] = json!(false);
    let path = passthrough(|| create_with(&dir.join("legacy.sqlite"), &settings, None)).unwrap();
    mutate(
        &path,
        "DELETE FROM context_items WHERE kind='native_config_extension' AND item_key='run'",
    );
    let before = fs::read(&path).unwrap();
    let con = passthrough(|| open_scan(&path, false, None)).unwrap();
    let effective = passthrough(|| read_effective_config(&con)).unwrap();
    assert_eq!(
        effective["scope"]["normalize_tracking_parameters"],
        json!(false)
    );
    drop(con);
    assert_eq!(fs::read(&path).unwrap(), before);
    // A different request is refused before any writer connection exists.
    refused(
        passthrough(|| NativeScanSchemaMixin::open(&path, None, Some(&config()), None, false)),
        "configuration differs",
    );
    let _ = fs::remove_dir_all(dir);
}

// Create preconditions.
#[test]
fn create_checks_identity_inputs() {
    let dir = scratch("identity");
    let target = dir.join("x.sqlite");
    let create_args = |start: &str, revision: &str, versions: Map<String, Value>, format: &str| {
        passthrough(|| {
            NativeScanSchemaMixin::create_file(
                &target,
                start,
                &config(),
                None,
                "3.0.0",
                revision,
                &versions,
                &[],
                None,
                &[],
                format,
            )
        })
    };
    refused(
        create_args("", &"a".repeat(40), versions(), "scan.v1"),
        "start_url is required",
    );
    refused(
        create_args("https://e.test/", "ABC", versions(), "scan.v1"),
        "40-character",
    );
    let mut incomplete = versions();
    incomplete.remove("lxml");
    refused(
        create_args("https://e.test/", &"a".repeat(40), incomplete, "scan.v1"),
        "runtime version provenance",
    );
    refused(
        create_args("https://e.test/", &"a".repeat(40), versions(), "scan.v3"),
        "scan.v1 or scan.v2",
    );
    refused(
        create_args("https://e.test/", &"a".repeat(40), versions(), "scan.v2"),
        "format selection differs",
    );
    refused(
        passthrough(|| create_with(&target, &config(), Some("0000000000000000"))),
        "fingerprint disagrees",
    );
    assert!(!target.exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn create_never_overwrites() {
    let dir = scratch("no-clobber");
    let path = create(&dir, "scan.sqlite");
    let before = fs::read(&path).unwrap();
    refused(
        passthrough(|| create_with(&path, &config(), None)),
        "already exists",
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let leftovers = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".native-scan-"))
        .count();
    assert_eq!(leftovers, 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn created_scan_is_a_published_delete_journal_file() {
    let dir = scratch("journal");
    let path = create(&dir, "scan.sqlite");
    let con = Connection::open(&path).unwrap();
    let mode: String = con
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    let (lifecycle, reason, fmt): (String, String, String) = con
        .query_row(
            "SELECT lifecycle, finish_reason, format_version FROM scan",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (lifecycle.as_str(), reason.as_str(), fmt.as_str()),
        ("running", "running", "scan.v1")
    );
    let inspected = passthrough(|| NativeScanSchemaMixin::inspect(&path, None)).unwrap();
    assert_eq!(inspected["counts"]["pages"], 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn light_open_still_rejects_schema_and_header_damage() {
    let cases = [
        "DROP INDEX pages_status",
        "UPDATE scan SET config_fingerprint='0000000000000000'",
        "DELETE FROM resume_state",
        "PRAGMA ignore_check_constraints=ON; UPDATE scan SET lifecycle='bogus'",
    ];
    for (index, mutation) in cases.iter().enumerate() {
        let dir = scratch(&format!("light-damage{index}"));
        let path = create(&dir, "scan.sqlite");
        mutate(&path, mutation);
        assert!(
            passthrough(|| open_scan_mode(&path, false, None, true)).is_err(),
            "{mutation}"
        );
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn light_open_rejects_non_scan_and_foreign_files() {
    let dir = scratch("light-foreign");
    let garbage = dir.join("garbage.sqlite");
    fs::write(&garbage, b"not a database".repeat(100)).unwrap();
    let foreign = dir.join("foreign.sqlite");
    Connection::open(&foreign)
        .unwrap()
        .execute_batch("CREATE TABLE t(x)")
        .unwrap();
    for path in [garbage, foreign, dir.join("missing.sqlite")] {
        assert!(
            passthrough(|| open_scan_mode(&path, false, None, true)).is_err(),
            "{path:?}"
        );
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn full_result_is_reused_for_unchanged_bytes_and_dropped_on_change() {
    let dir = scratch("reuse");
    let path = create(&dir, "scan.sqlite");
    let (c, first) = passthrough(|| open_scan_mode(&path, false, None, true)).unwrap();
    assert_eq!(first, "light");
    drop(c);
    drop(passthrough(|| open_scan(&path, false, None)).unwrap());
    let (c, reused) = passthrough(|| open_scan_mode(&path, false, None, true)).unwrap();
    assert_eq!(reused, "full");
    drop(c);
    mutate(&path, "UPDATE scan SET pinned=1");
    let (_c, after) = passthrough(|| open_scan_mode(&path, false, None, true)).unwrap();
    assert_eq!(after, "light");
    let _ = fs::remove_dir_all(dir);
}

// Adversarial persisted state: header and runtime rows.
#[test]
fn full_validation_refuses_damaged_persisted_state() {
    let cases = [
        ("UPDATE scan SET writer_revision='short'", "writer revision"),
        (
            "UPDATE scan SET runtime_versions_json='{}'",
            "runtime provenance",
        ),
        ("UPDATE scan SET limitations_json='[1]'", "string list"),
        ("UPDATE scan SET capabilities_json='{}'", "capability metadata"),
        (
            "UPDATE scan SET capabilities_json=json_set(capabilities_json,'$.responses.state','bogus')",
            "capability metadata",
        ),
        (
            "UPDATE scan SET capabilities_json=json_set(capabilities_json,'$.pages.state','unavailable')",
            "claims an unavailable feature",
        ),
        ("UPDATE scan SET retention_json='{}'", "retention|policy"),
        ("UPDATE scan SET corpus_partial=0", "complete bytes"),
        (
            "UPDATE resume_state SET throttle_state_json='nope'",
            "invalid JSON",
        ),
        (
            "UPDATE resume_state SET throttle_state_json='{}'",
            "runtime state",
        ),
        ("UPDATE resume_state SET elapsed_seconds=-1", "runtime state"),
        (
            "UPDATE resume_state SET circuit_timeout_streak=-1",
            "runtime state",
        ),
        ("UPDATE scan SET evidence_revision=3", "revision"),
        (
            "DELETE FROM context_items WHERE kind='credential_context'",
            "credential resume context",
        ),
        (
            "UPDATE scan SET parent_scan_uuid='00000000-0000-0000-0000-000000000000'",
            "parent",
        ),
        ("UPDATE scan SET scan_uuid='not-a-uuid'", "UUID|uuid"),
    ];
    for (index, (mutation, message)) in cases.iter().enumerate() {
        let dir = scratch(&format!("damage{index}"));
        let path = create(&dir, "scan.sqlite");
        mutate(&path, mutation);
        refused(
            passthrough(|| NativeScanSchemaMixin::inspect(&path, None)),
            message,
        );
        let _ = fs::remove_dir_all(dir);
    }
}

// Frontier and page rows must not disagree; storage-level check.
#[test]
fn frontier_rows_are_checked() {
    let seed = "INSERT INTO urls(url) VALUES ('https://example.test/');";
    let cases = [
        (
            "INSERT INTO frontier(url_id,state,depth,queue_ordinal) SELECT url_id,'bogus',0,0 FROM urls",
            "frontier",
        ),
        (
            "INSERT INTO frontier(url_id,state,depth,queue_ordinal) SELECT url_id,'queued',0,5 FROM urls",
            "contiguous",
        ),
        (
            "INSERT INTO frontier(url_id,state,depth,queue_ordinal) SELECT url_id,'queued',-1,0 FROM urls",
            "negative",
        ),
        (
            "INSERT INTO frontier(url_id,state,depth,queue_ordinal) SELECT url_id,'done',0,0 FROM urls",
            "frontier completion",
        ),
    ];
    for (index, (mutation, message)) in cases.iter().enumerate() {
        let dir = scratch(&format!("frontier{index}"));
        let path = create(&dir, "scan.sqlite");
        mutate(&path, &format!("{seed} {mutation}"));
        refused(
            passthrough(|| NativeScanSchemaMixin::inspect(&path, None)),
            message,
        );
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn inspect_timeout_must_be_positive_and_names_the_budget() {
    let dir = scratch("timeout");
    let path = create(&dir, "scan.sqlite");
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        refused(
            NativeScanSchemaMixin::inspect(&path, Some(bad)),
            "must be positive",
        );
    }
    assert_eq!(default_inspect_timeout(0), 30.0);
    assert_eq!(default_inspect_timeout(4_194_304 * 100), 100.0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn observe_bounds_and_identity() {
    let dir = scratch("observe");
    let path = create(&dir, "scan.sqlite");
    for bad in [0.0, 5.1, f64::NAN, -0.5] {
        refused(
            NativeScanSchemaMixin::observe(&path, bad),
            "at most 5 seconds",
        );
    }
    let observed = NativeScanSchemaMixin::observe(&path, 0.2).unwrap();
    assert_eq!(observed["validation"], "metadata_only");
    assert_eq!(
        observed["counts"],
        json!({"pages": 0, "queued": 0, "inflight": 0, "done": 0, "excluded": 0})
    );
    assert_eq!(observed["scan"]["lifecycle"], "running");
    mutate(&path, "PRAGMA application_id=1");
    refused(NativeScanSchemaMixin::observe(&path, 0.2), "identity");
    refused(
        NativeScanSchemaMixin::observe(&dir.join("missing.sqlite"), 0.2),
        "unavailable",
    );
    let _ = fs::remove_dir_all(dir);
}

// Symlink creation needs privilege on Windows.
#[test]
#[cfg(unix)]
fn writer_lock_is_exclusive_and_refuses_symlinks() {
    let dir = scratch("lock");
    let lock = dir.join("scan.sqlite.writer.lock");
    let first = WriterLock::acquire(&lock).unwrap();
    refused(WriterLock::acquire(&lock).map(|_| ()), "active writer");
    drop(first);
    drop(WriterLock::acquire(&lock).unwrap());
    let link = dir.join("link.lock");
    std::os::unix::fs::symlink(&lock, &link).unwrap();
    refused(
        WriterLock::acquire(&link).map(|_| ()),
        "symlink|Too many levels|cannot acquire",
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn open_refuses_aliases_and_missing_files() {
    let dir = scratch("open-guards");
    let path = create(&dir, "scan.sqlite");
    refused(
        NativeScanSchemaMixin::open(&dir.join("missing.sqlite"), None, None, None, false),
        "existing regular scan file",
    );
    fs::hard_link(&path, dir.join("alias.sqlite")).unwrap();
    refused(
        NativeScanSchemaMixin::open(&path, None, None, None, false),
        "hard-linked",
    );
    fs::remove_file(dir.join("alias.sqlite")).unwrap();
    refused(
        passthrough(|| {
            NativeScanSchemaMixin::open(&path, Some("https://other.test/"), None, None, false)
        }),
        "start URL differs",
    );
    refused(
        passthrough(|| {
            NativeScanSchemaMixin::open(&path, None, None, Some(&"b".repeat(40)), false)
        }),
        "producing build differs",
    );
    mutate(
        &path,
        "UPDATE scan SET lifecycle='finished', finished_at='2026-01-01T00:00:00Z'",
    );
    refused(
        passthrough(|| NativeScanSchemaMixin::open(&path, None, None, None, false)),
        "cannot be opened for writing",
    );
    let _ = fs::remove_dir_all(dir);
}

// Config header helpers.
#[test]
fn frozen_header_drops_the_extension_and_probe_settings() {
    let effective = passthrough(|| native_config(&Value::Object(config()), false)).unwrap();
    let header = frozen_header_config(&effective);
    for (path, _) in config_extension_defaults() {
        let mut node = Some(&Value::Object(header.clone()));
        for part in path.split('.') {
            node = node.and_then(|n| n.get(part));
        }
        assert!(node.is_none(), "{path}");
    }
    assert!(header["analysis"].get("asset_thresholds").is_none());
    assert!(header["analysis"].get("redirect_hop_hsts_probe").is_none());
    let values = config_extension_values(&effective).unwrap();
    assert_eq!(values.len(), config_extension_defaults().len());
}

#[test]
fn recorded_config_may_lack_options_added_later() {
    let effective = passthrough(|| native_config(&Value::Object(config()), false)).unwrap();
    let mut old = Value::Object(effective.clone());
    old.as_object_mut().unwrap().remove("fetch");
    old["limits"]
        .as_object_mut()
        .unwrap()
        .remove("max_requests");
    old["http"].as_object_mut().unwrap().remove("proxy");
    let recorded = passthrough(|| native_config(&old, true)).unwrap();
    assert!(!recorded.contains_key("fetch"));
    // A current request resumes against that shape when it sets the defaults.
    let wanted =
        passthrough(|| resume_fingerprint(&Value::Object(effective.clone()), &old)).unwrap();
    let again = passthrough(|| resume_fingerprint(&Value::Object(effective), &old)).unwrap();
    assert_eq!(wanted, again);
    // A missing required section is not a recorded-shape difference.
    let mut broken = old.clone();
    broken.as_object_mut().unwrap().remove("speed");
    refused(
        passthrough(|| native_config(&broken, true)),
        "complete resolved",
    );
}

#[test]
fn legacy_set_cookie_latch_needs_credential_free_anonymous_config() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE responses(response_headers_redacted_json TEXT, effective_headers_redacted_json TEXT);",
    )
    .unwrap();
    let header = crate::crawl_capture::REDACTED_NAMES_HEADER;
    con.execute(
        "INSERT INTO responses VALUES (?1, NULL)",
        [serde_json::to_string(&json!([[header, "set-cookie,x"]])).unwrap()],
    )
    .unwrap();
    let anonymous = json!({"http": {"credential_headers": {}}, "rendering": {"browser": {"persistent_profile": false}}});
    assert!(legacy_anonymous_session(anonymous.as_object().unwrap(), &con).unwrap());
    let credentialed = json!({"http": {"credential_headers": {"a": "b"}}, "rendering": {"browser": {"persistent_profile": false}}});
    assert!(!legacy_anonymous_session(credentialed.as_object().unwrap(), &con).unwrap());
    let persistent = json!({"http": {"credential_headers": {}}, "rendering": {"browser": {"persistent_profile": true}}});
    assert!(!legacy_anonymous_session(persistent.as_object().unwrap(), &con).unwrap());
    assert!(!legacy_anonymous_session(json!({}).as_object().unwrap(), &con).unwrap());
}

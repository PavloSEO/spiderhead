//! Ledger schema, identity and bootstrap tests that need no source scan.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::json;
use spiderhead_store::ledger::ingest::{
    affected_urls, coverage_state, create_ledger, occurrence_specs, validated_time,
};
use spiderhead_store::ledger::queries::ledger_summary;
use spiderhead_store::ledger::schema::{
    canonical_url, finding_key, occurrence_key, open_ledger, APPLICATION_ID,
};
use spiderhead_store::ledger::LedgerRef;

const BUILD: &str = "25fd2ed032a31d63c5811722619e35c14b631476";
const PROJECT: &str = "0d6f2c1e-8c1a-4a0c-9d6e-2b7f6a1f9f10";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-ledger-units-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha(path: &Path) -> String {
    spiderhead_core::filesystem::file_sha256(path).unwrap()
}

fn make(dir: &Path) -> PathBuf {
    create_ledger(
        &dir.join("ledger.sqlite"),
        PROJECT,
        "https://example.test/",
        "3.2.0",
        BUILD,
    )
    .unwrap()
}

#[test]
fn create_binds_project_and_never_overwrites() {
    let dir = scratch("create");
    let ledger = make(&dir);
    let summary = ledger_summary(LedgerRef::Path(&ledger)).unwrap();
    assert_eq!(summary["format_version"], "ledger.v1");
    assert_eq!(summary["ledger_revision"], 0);
    assert_eq!(summary["sites"][0]["role"], "primary");
    assert_eq!(summary["sites"][0]["host"], "example.test");
    let again =
        create_ledger(&ledger, PROJECT, "https://example.test/", "3.2.0", BUILD).unwrap_err();
    assert!(again.to_string().contains("never overwrite"), "{again}");
    let bad = create_ledger(
        &dir.join("third.sqlite"),
        PROJECT,
        "https://example.test/",
        "3.2.0",
        "not-a-sha",
    )
    .unwrap_err();
    assert!(bad.to_string().contains("Git SHA"), "{bad}");
    assert!(!dir.join("third.sqlite").exists());
    let missing_parent = create_ledger(
        &dir.join("no/such/ledger.sqlite"),
        PROJECT,
        "https://example.test/",
        "3.2.0",
        BUILD,
    );
    assert!(missing_parent.is_err());
    // no temporary file is left behind
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".ledger-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn future_versions_refuse_without_mutation() {
    let dir = scratch("future");
    let ledger = make(&dir);
    let con = Connection::open(&ledger).unwrap();
    con.execute_batch("PRAGMA user_version=6").unwrap();
    drop(con);
    let digest = sha(&ledger);
    for write in [false, true] {
        let err = open_ledger(&ledger, write).unwrap_err();
        assert!(err.to_string().contains("user_version"), "{err}");
        assert_eq!(sha(&ledger), digest);
    }
    // a foreign application id is refused too
    let con = Connection::open(&ledger).unwrap();
    con.execute_batch(&format!(
        "PRAGMA user_version=5; PRAGMA application_id={}",
        APPLICATION_ID + 1
    ))
    .unwrap();
    drop(con);
    assert!(open_ledger(&ledger, false)
        .unwrap_err()
        .to_string()
        .contains("foreign application_id"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn v1_ledger_migrates_only_on_a_write_open() {
    let dir = scratch("v1");
    let path = dir.join("v1.sqlite");
    let con = Connection::open(&path).unwrap();
    con.execute_batch(&spiderhead_store::ledger::schema::schema())
        .unwrap();
    con.execute(
        "INSERT INTO ledger VALUES(1,?,?,?,?,?,?)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            "ledger.v1",
            "2026-10-01T00:00:00Z",
            "test",
            BUILD,
            0
        ],
    )
    .unwrap();
    drop(con);
    let original = sha(&path);
    let err = open_ledger(&path, false).unwrap_err();
    assert!(err.to_string().contains("user_version 1"), "{err}");
    assert_eq!(sha(&path), original);
    let upgraded = open_ledger(&path, true).unwrap();
    let version: i64 = upgraded
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 5);
    let format: String = upgraded
        .query_row("SELECT format_version FROM ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(format, "ledger.v1");
    let artifacts: i64 = upgraded
        .query_row("SELECT COUNT(*) FROM verification_artifact", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(artifacts, 0);
    drop(upgraded);
    drop(open_ledger(&path, false).unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

fn v0_stub(path: &Path, project_uuid: &str, target: &str) {
    let con = Connection::open(path).unwrap();
    con.execute_batch(&format!(
        "PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version=0"
    ))
    .unwrap();
    con.execute_batch(
        "CREATE TABLE ledger_meta(singleton INTEGER PRIMARY KEY CHECK (singleton=1),format_version TEXT NOT NULL,ledger_uuid TEXT NOT NULL,project_uuid TEXT NOT NULL,site_target TEXT NOT NULL,writer_version TEXT NOT NULL,writer_revision TEXT NOT NULL,created_at TEXT NOT NULL)",
    )
    .unwrap();
    con.execute(
        "INSERT INTO ledger_meta VALUES(1,'ledger.v0',?,?,?,?,?,?)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            project_uuid,
            target,
            "3.2.0",
            BUILD,
            "2026-09-01T00:00:00Z"
        ],
    )
    .unwrap();
}

#[test]
fn v0_stub_migrates_on_explicit_write() {
    let dir = scratch("v0");
    let path = dir.join("stub.sqlite");
    v0_stub(&path, PROJECT, "https://example.test/");
    assert!(open_ledger(&path, false)
        .unwrap_err()
        .to_string()
        .contains("user_version"));
    let con = open_ledger(&path, true).unwrap();
    let integrity: String = con
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let primaries: i64 = con
        .query_row("SELECT COUNT(*) FROM site WHERE role='primary'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(primaries, 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn failed_migration_rolls_back() {
    let dir = scratch("v0bad");
    let path = dir.join("bad-stub.sqlite");
    v0_stub(&path, "not-a-uuid", "https://example.test/");
    let digest = sha(&path);
    assert!(open_ledger(&path, true).is_err());
    assert_eq!(sha(&path), digest);
    let con = Connection::open(&path).unwrap();
    let version: i64 = con
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let format: String = con
        .query_row("SELECT format_version FROM ledger_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(format, "ledger.v0");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tampered_rows_fail_validation() {
    let dir = scratch("tamper");
    let ledger = make(&dir);
    let con = Connection::open(&ledger).unwrap();
    con.execute("UPDATE site SET host='other.test'", [])
        .unwrap();
    drop(con);
    // the host is not part of the netloc check, but a changed netloc is
    let con = Connection::open(&ledger).unwrap();
    con.execute("UPDATE site SET netloc='evil.test'", [])
        .unwrap();
    drop(con);
    assert!(open_ledger(&ledger, false)
        .unwrap_err()
        .to_string()
        .contains("site row identity"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn identity_keys_are_stable_and_distinct() {
    let uuid = PROJECT;
    let subject = canonical_url("https://example.test/a/").unwrap();
    assert_eq!(subject, "https://example.test/a");
    let key = finding_key(uuid, "example.test", "CHECK_ONE", "url", &subject);
    assert_eq!(
        key,
        finding_key(uuid, "example.test", "CHECK_ONE", "url", &subject)
    );
    assert_ne!(
        key,
        finding_key(uuid, "example.test", "CHECK_TWO", "url", &subject)
    );
    assert_ne!(
        key,
        finding_key(uuid, "example.test", "CHECK_ONE", "scope", "site")
    );
    let occ = |rep: &str, dt: &str, dv: &str| {
        occurrence_key(
            uuid,
            "example.test",
            "CHECK_ONE",
            "url",
            &subject,
            rep,
            dt,
            dv,
        )
    };
    assert_eq!(occ("static", "primary", ""), occ("static", "primary", ""));
    assert_ne!(occ("static", "primary", ""), occ("rendered", "primary", ""));
    assert_ne!(occ("static", "primary", ""), occ("static", "subject", "x"));
    assert_eq!(key.len(), 64);
    // the key is the SHA-256 of the compact JSON list (non-ASCII kept)
    assert_eq!(finding_key("u", "h", "c", "url", "é"), {
        use sha2::{Digest, Sha256};
        let text = r#"["spiderhead.ledger-finding.v1","u","h","c","url","é"]"#;
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    });
}

#[test]
fn canonical_url_applies_the_audit_policy() {
    assert_eq!(
        canonical_url(" HTTPS://Example.TEST/a/?q=1#f ").unwrap(),
        "https://example.test/a?q=1#f"
    );
    assert!(canonical_url("").is_err());
    assert!(canonical_url("   ").is_err());
    assert!(canonical_url(&format!("https://example.test/{}", "a".repeat(2100))).is_err());
}

#[test]
fn repeated_same_url_locations_keep_stable_link_path_occurrences() {
    let a = "https://example.test/a";
    let issue = json!({
        "id": "ISSUE-000001", "check": "CHECK_ONE", "target_url": a, "occurrences_count": 2,
        "locations": [
            {"url": a, "link_path": "main > a:nth-child(1)", "representation": "static"},
            {"url": a, "link_path": "main > a:nth-child(2)", "representation": "rendered"},
        ],
    });
    let members = affected_urls(&issue).unwrap();
    assert_eq!(
        members,
        vec![
            (a.to_string(), "subject".to_string()),
            (a.to_string(), "location".to_string())
        ]
    );
    let specs = occurrence_specs(&issue, "url", a, &members, &BTreeMap::new());
    assert_eq!(specs.len(), 2);
    assert!(specs
        .iter()
        .all(|s| s.discriminator_type == "locator" && s.role == "target"));
    assert_ne!(specs[0].discriminator_value, specs[1].discriminator_value);
    assert_eq!(
        [
            specs[0].representation.as_str(),
            specs[1].representation.as_str()
        ],
        ["static", "rendered"]
    );
    let locations = issue["locations"].as_array().unwrap().clone();
    assert_eq!(
        coverage_state(&issue, &locations),
        ("enumerated".to_string(), 2)
    );
}

#[test]
fn repeated_same_url_without_locators_stays_aggregate() {
    let a = "https://example.test/a";
    let issue = json!({
        "id": "ISSUE-000001", "check": "CHECK_ONE", "target_url": a, "occurrences_count": 3,
        "locations": [{"url": a}, {"url": a}],
    });
    let members = affected_urls(&issue).unwrap();
    let specs = occurrence_specs(&issue, "url", a, &members, &BTreeMap::new());
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].discriminator_type, "primary");
    assert_eq!(specs[0].representation, "unknown");
    let locations = issue["locations"].as_array().unwrap().clone();
    // reported 3 > 2 listed locations: capped
    assert_eq!(coverage_state(&issue, &locations).0, "capped");
    let scope = json!({"id": "I", "check": "C", "occurrences_count": 4});
    let scope_specs = occurrence_specs(&scope, "scope", "site", &[], &BTreeMap::new());
    assert_eq!(scope_specs.len(), 1);
    assert_eq!(scope_specs[0].representation, "scope");
    assert_eq!(coverage_state(&scope, &[]).0, "aggregate_only");
}

#[test]
fn timestamps_are_recorded_only_with_a_zero_offset() {
    for ok in [
        "2026-10-01T00:00:00Z",
        "2026-10-01T00:00:00+00:00",
        "2026-10-01 00:00:00.123456Z",
    ] {
        assert_eq!(validated_time(&json!(ok)).as_deref(), Some(ok));
    }
    for bad in [
        "",
        "2026-10-01T00:00:00",
        "2026-10-01T00:00:00+02:00",
        "yesterday",
        "2026-02-30T00:00:00Z",
        "2026-13-01T00:00:00Z",
    ] {
        assert_eq!(validated_time(&json!(bad)), None, "{bad}");
    }
    assert_eq!(validated_time(&json!(5)), None);
}

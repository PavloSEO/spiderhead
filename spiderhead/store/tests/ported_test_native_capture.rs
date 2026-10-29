//! Ports of the reference suite that the Rust `NativeScan` writer can express.
//!
//! Not ported here (see the slice report): the disk-full rollback needs a failpoint that
//! returns an error (the Rust hook only observes); the preflight and free-space tests patch
//! `shutil.disk_usage`; `retained_start_gate` belongs to the crawl adapter; the read-only
//! document test needs `read_document`, which is private to `corpus_inputs`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{load, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_store::crawl_capture::CaptureEvent;
use spiderhead_store::error::StoreError;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::{Lease, NativeScanLease, PageCommit};
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

const ROOT: &str = "https://example.test/";
const START: &str = "https://example.test/";

/// A scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sh-native-capture-{}-{serial}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn scan_path(&self) -> PathBuf {
        self.0.join("scan.sqlite")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn message(e: StoreError) -> String {
    match e {
        StoreError::ScanError(s) => s.message,
        StoreError::Invalid(m) => m,
        other => other.to_string(),
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

/// Settings as reference `load(overrides={"speed.min_delay_seconds": 0, **overrides})`.
fn config_with_env(overrides: &[(&str, Value)], host: &FakeHost) -> Map<String, Value> {
    let mut merged = Map::new();
    merged.insert("speed.min_delay_seconds".to_owned(), json!(0));
    for (key, value) in overrides {
        merged.insert((*key).to_owned(), value.clone());
    }
    let resolved = load(
        &LoadInput {
            overrides: merged,
            ..LoadInput::default()
        },
        host,
    )
    .expect("settings should load");
    object(resolved)
}

fn config(overrides: &[(&str, Value)]) -> Map<String, Value> {
    config_with_env(overrides, &FakeHost::default())
}

fn runtime_versions() -> Map<String, Value> {
    object(json!({
        "python": "test",
        "sqlite": "test",
        "httpx": "test",
        "lxml": "test",
        "beautifulsoup4": "test",
    }))
}

/// reference `_metadata(**overrides)` turned into `NativeScan::create` arguments.
fn create_with(path: &Path, config: &Map<String, Value>) -> NativeScan {
    NativeScanSchemaMixin::create(
        path,
        START,
        config,
        None,
        "3.0.0",
        &"a".repeat(40),
        &runtime_versions(),
        &[],
        None,
        &[],
        "scan.v1",
    )
    .expect("create native scan")
}

fn create(path: &Path, overrides: &[(&str, Value)]) -> NativeScan {
    create_with(path, &config(overrides))
}

/// reference `_runtime()`.
fn runtime() -> Map<String, Value> {
    object(json!({
        "max_depth_reached": 1,
        "elapsed_seconds": 0.0,
        "circuit_timeout_streak": 0,
        "circuit_server_error_streak": 0,
        "crawl_delay_applied": null,
        "throttle": {"delay_seconds": 0.0, "concurrency": 1, "consecutive_ok": 0},
    }))
}

/// reference `_record(url)`: the commit-record shape taken from the reference baseline scenario `a`
/// (`commit-expected.json`), with the title, type and depth `_record` sets.
fn record(url: &str) -> Map<String, Value> {
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("commit-expected.json")).unwrap(),
    )
    .unwrap();
    let first_commit = expected["a"]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["op"] == "commit")
        .unwrap();
    let mut base = first_commit["record"].as_object().unwrap().clone();
    base.remove("bogus");
    base.insert("url".into(), json!(url));
    base.insert("title".into(), json!("Home"));
    base.insert("content_type".into(), json!("text/html"));
    base.insert("crawl_depth".into(), json!(if url == ROOT { 0 } else { 1 }));
    base
}

/// reference `_link(source, destination)`.
fn link(source: &str, destination: &str) -> Map<String, Value> {
    object(json!({
        "source": source,
        "destination": destination,
        "anchor": "",
        "nofollow": false,
        "position": "",
        "rel": [],
        "target": "",
        "raw_href": "",
    }))
}

/// reference `_event(url, body, **changes)`: one captured HTML response.
fn event(url: &str, body: &[u8], changes: &[(&str, Value)]) -> CaptureEvent {
    let mut values = json!({
        "method": "GET",
        "requested_url": url,
        "effective_url": url,
        "redirect_history": [],
        "requested_at": "2026-09-06T10:00:00Z",
        "received_at": "2026-09-06T10:00:01Z",
        "status_code": 200,
        "request_headers": [["accept", "text/html"]],
        "credentials_used": false,
        "response_headers": [["content-type", "text/html; charset=utf-8"]],
        "content_type": "text/html; charset=utf-8",
        "content_encoding": "",
        "entity_bytes": body,
        "body_fidelity": "entity_bytes",
        "body_state": "complete",
        "body_reason": "none",
        "error": "",
        "error_kind": "",
        "effective_status_code": 200,
        "effective_headers": [["content-type", "text/html; charset=utf-8"]],
        "response_time": 0.1,
        "session_changed": false,
        "http_version": null,
        "timing_state": "unavailable",
        "transport_phases": null,
    });
    for (key, value) in changes {
        values[*key] = value.clone();
    }
    serde_json::from_value(values).expect("capture event")
}

/// reference `_claim(scan, url)`: enqueue one URL and take its lease.
fn claim(scan: &NativeScan, url: &str) -> Lease {
    let depth = if url.ends_with('/') { 0 } else { 1 };
    scan.enqueue(&[(url.to_owned(), depth)], "enqueue")
        .expect("enqueue");
    scan.claim(1).expect("claim").remove(0)
}

fn count(con: &Connection, sql: &str) -> i64 {
    con.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn text(con: &Connection, sql: &str) -> String {
    con.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn commit(lease: &Lease, record: Map<String, Value>) -> PageCommit {
    PageCommit {
        lease: Some(lease.clone()),
        record,
        runtime: Some(runtime()),
        ..Default::default()
    }
}

#[test]
fn native_capture_deduplicates_bodies_and_keeps_document_lineage() {
    let dir = Scratch::new("dedupe");
    let mut scan = create(&dir.scan_path(), &[]);
    let shared: &[u8] = b"<html><body>same retained entity</body></html>";
    let first = claim(&scan, ROOT);
    scan.enqueue(&[("https://example.test/second".to_owned(), 1)], "enqueue")
        .unwrap();
    let first_commit = PageCommit {
        links: vec![link(&first.url, "https://example.test/linked")],
        forms: vec![object(json!({
            "page": first.url,
            "method": "get",
            "action": "/search",
            "has_password": false,
        }))],
        captures: vec![event(&first.url, shared, &[])],
        ..commit(&first, record(&first.url))
    };
    scan.commit_page(&first_commit).unwrap();

    let second = scan.claim(1).unwrap().remove(0);
    let mut second_runtime = runtime();
    second_runtime.insert("max_depth_reached".into(), json!(1));
    let second_commit = PageCommit {
        captures: vec![event(&second.url, shared, &[])],
        runtime: Some(second_runtime),
        ..commit(&second, record(&second.url))
    };
    scan.commit_page(&second_commit).unwrap();

    let con = scan.con();
    assert_eq!(count(con, "SELECT COUNT(*) FROM bodies"), 1);
    let mut stmt = con
        .prepare("SELECT document_id FROM pages ORDER BY page_ordinal")
        .unwrap();
    let document_ids: Vec<Option<i64>> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(document_ids.len() == 2 && document_ids.iter().all(|id| id.is_some()));
    let first_document = document_ids[0];
    assert_eq!(
        con.query_row("SELECT source_document_id FROM links", [], |row| row
            .get::<_, Option<i64>>(0))
            .unwrap(),
        first_document
    );
    assert_eq!(
        con.query_row("SELECT source_document_id FROM forms", [], |row| row
            .get::<_, Option<i64>>(0))
            .unwrap(),
        first_document
    );
    assert_eq!(
        count(con, "SELECT COUNT(DISTINCT body_sha256) FROM documents"),
        1
    );
}

#[test]
fn exact_capture_retry_is_idempotent_with_one_response_document_and_body() {
    let dir = Scratch::new("retry");
    let mut scan = create(&dir.scan_path(), &[]);
    let lease = claim(&scan, ROOT);
    let page = PageCommit {
        captures: vec![event(
            &lease.url,
            b"<html><title>Captured</title></html>",
            &[],
        )],
        ..commit(&lease, record(&lease.url))
    };
    let first = scan.commit_page(&page).unwrap();
    let retry = scan.commit_page(&page).unwrap();
    assert!(!first.already_committed && retry.already_committed);
    let con = scan.con();
    let counts: (i64, i64, i64) = con
        .query_row(
            "SELECT COUNT(*), (SELECT COUNT(*) FROM documents), (SELECT COUNT(*) FROM bodies) FROM responses",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 1, 1));
}

/// Shared body of the capability tests: one committed page, then the capability and the
/// response's body state as reference reads them. `expected` is
/// `(html_bodies state, html_bodies reason, response body state, response body reason)`.
fn capability_case(
    overrides: &[(&str, Value)],
    event_changes: &[(&str, Value)],
    expected: (&str, &str, &str, &str),
) {
    let dir = Scratch::new("capabilities");
    let mut scan = create(&dir.scan_path(), overrides);
    let lease = claim(&scan, ROOT);
    let page = PageCommit {
        captures: vec![event(
            &lease.url,
            b"<html><title>Captured</title></html>",
            event_changes,
        )],
        ..commit(&lease, record(&lease.url))
    };
    scan.commit_page(&page).unwrap();
    let con = scan.con();
    let capabilities: Value =
        serde_json::from_str(&text(con, "SELECT capabilities_json FROM scan")).unwrap();
    let (body_state, body_reason): (String, String) = con
        .query_row("SELECT body_state,body_reason FROM responses", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();

    let (html_state, html_reason, want_state, want_reason) = expected;
    assert_eq!(
        capabilities["responses"],
        json!({"state": "complete", "reason": ""})
    );
    assert_eq!(
        capabilities["html_bodies"],
        json!({"state": html_state, "reason": html_reason})
    );
    assert_eq!(
        (body_state.as_str(), body_reason.as_str()),
        (want_state, want_reason)
    );
}

#[test]
fn capabilities_captured() {
    capability_case(&[], &[], ("complete", "", "complete", "none"));
}

#[test]
fn capabilities_retention_off() {
    capability_case(
        &[("storage.body_mode", json!("off"))],
        &[],
        (
            "unavailable",
            "body retention is disabled",
            "omitted",
            "not_enabled",
        ),
    );
}

#[test]
fn capabilities_credentialed_omission() {
    capability_case(
        &[],
        &[("credentials_used", json!(true))],
        (
            "partial",
            "some HTML documents are missing or not retained",
            "omitted",
            "credentialed",
        ),
    );
}

#[test]
fn effective_store_budget_can_disable_new_body_storage() {
    let dir = Scratch::new("budget");
    let mut scan = create(
        &dir.scan_path(),
        &[("storage.max_body_store_bytes", json!(0))],
    );
    let lease = claim(&scan, ROOT);
    let page = PageCommit {
        captures: vec![event(
            &lease.url,
            b"<html><title>Captured</title></html>",
            &[],
        )],
        ..commit(&lease, record(&lease.url))
    };
    scan.commit_page(&page).unwrap();
    let con = scan.con();
    let (state, reason): (String, String) = con
        .query_row("SELECT body_state,body_reason FROM documents", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("omitted", "body_budget_exhausted")
    );
    assert_eq!(count(con, "SELECT COUNT(*) FROM bodies"), 0);
}

/// Shared body of the no-store retention tests.
fn no_store_case(overrides: &[(&str, Value)], credentials_used: bool, expected: (&str, &str)) {
    let no_store = json!([["cache-control", "no-store"]]);
    let dir = Scratch::new("no-store");
    let mut changes = vec![
        ("response_headers", no_store.clone()),
        ("effective_headers", no_store),
    ];
    if credentials_used {
        changes.push(("credentials_used", json!(true)));
    }
    let mut scan = create(&dir.scan_path(), overrides);
    let lease = claim(&scan, ROOT);
    let page = PageCommit {
        captures: vec![event(
            &lease.url,
            b"<html><title>Captured</title></html>",
            &changes,
        )],
        ..commit(&lease, record(&lease.url))
    };
    scan.commit_page(&page).unwrap();
    let con = scan.con();
    let (state, reason): (String, String) = con
        .query_row("SELECT body_state,body_reason FROM responses", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!((state.as_str(), reason.as_str()), expected);
    let config: Value = serde_json::from_str(&text(con, "SELECT config_json FROM scan")).unwrap();
    let retention: Value =
        serde_json::from_str(&text(con, "SELECT retention_json FROM scan")).unwrap();
    let acknowledged = overrides
        .iter()
        .find(|(key, _)| *key == "evidence.retain_no_store_acknowledged")
        .map(|(_, value)| value.as_bool().unwrap())
        .unwrap_or(false);
    assert_eq!(
        config["evidence"]["retain_no_store_acknowledged"],
        json!(acknowledged)
    );
    let mut keys: Vec<&str> = retention
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "automatic_delete",
            "body_mode",
            "history_warning_bytes",
            "max_body_bytes",
            "max_body_store_bytes",
            "min_free_bytes",
            "policy_version",
        ]
    );
}

#[test]
fn no_store_default_omits() {
    no_store_case(&[], false, ("omitted", "cache_control_no_store"));
}

#[test]
fn no_store_acknowledged_retains() {
    no_store_case(
        &[("evidence.retain_no_store_acknowledged", json!(true))],
        false,
        ("complete", "none"),
    );
}

#[test]
fn no_store_credentialed_still_omits() {
    no_store_case(
        &[("evidence.retain_no_store_acknowledged", json!(true))],
        true,
        ("omitted", "credentialed"),
    );
}

#[test]
fn history_policy_is_recorded_without_deleting_other_files() {
    let dir = Scratch::new("history");
    let older = dir.0.join("older.sqlite");
    std::fs::write(&older, b"retained history fixture").unwrap();
    {
        let scan = create(
            &dir.scan_path(),
            &[("storage.history_warning_bytes", json!(1))],
        );
        let retention: Value =
            serde_json::from_str(&text(scan.con(), "SELECT retention_json FROM scan")).unwrap();
        assert_eq!(retention["history_warning_bytes"], json!(1));
        assert_eq!(retention["automatic_delete"], json!(false));
    }
    assert_eq!(std::fs::read(&older).unwrap(), b"retained history fixture");
}

#[test]
fn persistent_profile_context_refuses_resume() {
    let dir = Scratch::new("profile");
    let profile = dir.0.join("profile");
    let cfg = config(&[
        ("rendering.browser.persistent_profile", json!(true)),
        (
            "rendering.browser.persistent_profile_dir",
            json!(profile.to_string_lossy()),
        ),
    ]);
    {
        let mut scan = create_with(&dir.scan_path(), &cfg);
        let lease = claim(&scan, ROOT);
        let page = PageCommit {
            captures: vec![event(&lease.url, b"<html></html>", &[])],
            ..commit(&lease, record(&lease.url))
        };
        scan.commit_page(&page).unwrap();
    }
    let refused =
        match NativeScanSchemaMixin::open(&dir.scan_path(), Some(START), Some(&cfg), None, false) {
            Ok(_) => panic!("resume of a persistent-profile scan must be refused"),
            Err(e) => message(e),
        };
    assert!(
        refused.contains("credentialed or persistent browser state"),
        "{refused}"
    );
}

#[test]
fn configured_credential_context_refuses_resume() {
    let dir = Scratch::new("credential");
    let secret_name = "SEOHEAD_SESSION_CHANGED_TOKEN";
    let host = FakeHost {
        env: [(secret_name.to_owned(), "Bearer stable-secret".to_owned())]
            .into_iter()
            .collect(),
        ..FakeHost::default()
    };
    // reference monkeypatch.setenv: the native writer reads the process environment. The name is
    // unique to this test, so no other test observes it.
    std::env::set_var(secret_name, "Bearer stable-secret");
    let cfg = config_with_env(
        &[
            (
                "http.credential_headers",
                json!([{"host": "example.test", "headers": {"Authorization": format!("env:{secret_name}")}}]),
            ),
            ("http.credentials_acknowledged", json!(true)),
        ],
        &host,
    );
    {
        let mut scan = create_with(&dir.scan_path(), &cfg);
        let lease = claim(&scan, ROOT);
        let changed = event(
            &lease.url,
            b"<html></html>",
            &[
                ("session_changed", json!(true)),
                (
                    "response_headers",
                    json!([["x-seohead-redacted-headers", "set-cookie"]]),
                ),
                (
                    "effective_headers",
                    json!([["x-seohead-redacted-headers", "set-cookie"]]),
                ),
            ],
        );
        let page = PageCommit {
            captures: vec![changed],
            ..commit(&lease, record(&lease.url))
        };
        scan.commit_page(&page).unwrap();
        let context: Value = serde_json::from_str(&text(
            scan.con(),
            "SELECT payload_json FROM context_items WHERE kind='credential_context' AND item_key='run'",
        ))
        .unwrap();
        assert_eq!(context["implicit_state"], json!(true));
    }
    let refused =
        match NativeScanSchemaMixin::open(&dir.scan_path(), Some(START), Some(&cfg), None, false) {
            Ok(_) => panic!("resume of a credential-context scan must be refused"),
            Err(e) => message(e),
        };
    assert!(
        refused.contains("credentialed or persistent browser state"),
        "{refused}"
    );
}

#[test]
fn unexplained_legacy_implicit_latch_remains_refused() {
    let dir = Scratch::new("latch");
    let cfg = config(&[]);
    {
        let mut scan = create_with(&dir.scan_path(), &cfg);
        let lease = claim(&scan, ROOT);
        let page = PageCommit {
            captures: vec![event(&lease.url, b"<html></html>", &[])],
            ..commit(&lease, record(&lease.url))
        };
        scan.commit_page(&page).unwrap();
        let mut context: Value = serde_json::from_str(&text(
            scan.con(),
            "SELECT payload_json FROM context_items WHERE kind='credential_context' AND item_key='run'",
        ))
        .unwrap();
        context["implicit_state"] = json!(true);
        scan.con()
            .execute(
                "UPDATE context_items SET payload_json=?1 WHERE kind='credential_context' AND item_key='run'",
                [context.to_string()],
            )
            .unwrap();
    }
    let refused =
        match NativeScanSchemaMixin::open(&dir.scan_path(), Some(START), Some(&cfg), None, false) {
            Ok(_) => panic!("an unexplained implicit-state latch must be refused"),
            Err(e) => message(e),
        };
    assert!(
        refused.contains("credentialed or persistent browser state"),
        "{refused}"
    );
}

#[test]
fn anonymous_server_cookie_allows_resume_with_fresh_session_provenance() {
    let dir = Scratch::new("anonymous");
    let cfg = config(&[]);
    {
        let mut scan = create_with(&dir.scan_path(), &cfg);
        let lease = claim(&scan, ROOT);
        let page = PageCommit {
            captures: vec![event(
                &lease.url,
                b"<html></html>",
                &[("session_changed", json!(true))],
            )],
            ..commit(&lease, record(&lease.url))
        };
        scan.commit_page(&page).unwrap();
    }
    let scan = NativeScanSchemaMixin::open(&dir.scan_path(), Some(START), Some(&cfg), None, false)
        .expect("an anonymous server cookie must not block resume");
    let provenance: Value = serde_json::from_str(&text(
        scan.con(),
        "SELECT payload_json FROM context_items WHERE kind='anonymous_session_resume' AND item_key='run'",
    ))
    .unwrap();
    let mut stmt = scan
        .con()
        .prepare("SELECT payload_json FROM context_items")
        .unwrap();
    let raw_context: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(provenance, json!({"fresh_session": true}));
    assert!(!raw_context.join("\n").contains("PHPSESSID"));
    // reference reads `inspect(path)["counts"]["pages"]`; the same count is read from the table.
    assert_eq!(count(scan.con(), "SELECT COUNT(*) FROM pages"), 1);
}

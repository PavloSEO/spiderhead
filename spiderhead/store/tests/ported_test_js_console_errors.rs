//! (pages with JavaScript errors, #1015; console messages of every type, #1475).
//!
//! Each test builds its own in-memory SQLite context table and a scratch scan directory, so no
//! network and no shared state is involved.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use spiderhead_store::browser_artifacts::{console_error_pages, retained_console, root, save};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A scratch directory removed on drop (the reference tests use `tmp_path`).
struct TmpDir(PathBuf);

impl TmpDir {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-js-console-{}-{name}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    /// The scan database path inside the scratch directory (the file itself need not exist).
    fn scan(&self) -> PathBuf {
        self.0.join("scan.sqlite")
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The reference `_connection()` fixture: urls, documents and the context table.
fn connection() -> Connection {
    let con = Connection::open_in_memory().expect("open in-memory database");
    con.execute_batch(
        "CREATE TABLE urls(url_id INTEGER PRIMARY KEY, url TEXT);
         CREATE TABLE documents(document_id INTEGER PRIMARY KEY, url_id INTEGER);
         CREATE TABLE context_items(kind TEXT, item_key TEXT, payload_json TEXT,
             PRIMARY KEY(kind, item_key));
         INSERT INTO urls VALUES(1, 'https://example.com/a');
         INSERT INTO urls VALUES(3, 'https://example.com/b');
         INSERT INTO documents VALUES(2, 1);
         INSERT INTO documents VALUES(4, 3);",
    )
    .expect("create fixture tables");
    con
}

/// The reference `_store()` helper: save one render's artifacts and record the context item.
fn store(con: &Connection, scan: &Path, page_url_id: i64, document_id: i64, rendered: Value) {
    let item = save(scan, page_url_id, document_id, &rendered, false, true)
        .expect("save browser artifacts");
    let text = |key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("context item lacks {key}"))
            .to_string()
    };
    con.execute(
        "INSERT INTO context_items VALUES(?1,?2,?3)",
        params![text("kind"), text("item_key"), text("payload_json")],
    )
    .expect("insert context item");
}

/// The single stored context payload of a fixture holding one render.
fn only_payload(con: &Connection) -> Value {
    let raw: String = con
        .query_row("SELECT payload_json FROM context_items", [], |row| {
            row.get(0)
        })
        .expect("one context item");
    serde_json::from_str(&raw).expect("payload is JSON")
}

/// The sidecar body the payload's content address points to (`_sidecar` in reference).
fn sidecar_body(scan: &Path, payload: &Value) -> Value {
    let digest = payload["console"]["ref"]["sha256"]
        .as_str()
        .expect("payload carries a sidecar digest");
    let text = std::fs::read_to_string(root(scan).join("console").join(format!("{digest}.json")))
        .expect("sidecar exists");
    serde_json::from_str(&text).expect("sidecar is JSON")
}

/// Path of the sidecar a payload points to, for the tamper tests.
fn sidecar_path(scan: &Path, payload: &Value) -> PathBuf {
    let digest = payload["console"]["ref"]["sha256"]
        .as_str()
        .expect("payload carries a sidecar digest");
    root(scan).join("console").join(format!("{digest}.json"))
}

/// A console message as the render reports it (`_message` in reference).
fn message(kind: &str, text: &str, location: Value) -> Value {
    json!({"type": kind, "text": text, "location": location})
}

#[test]
fn pages_with_console_errors_are_reported_and_clean_pages_are_not() {
    let dir = TmpDir::new("reported");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({"ok": true, "console_errors": ["Uncaught TypeError: x"]}),
    );
    store(&con, &scan, 3, 4, json!({"ok": true, "console_errors": []}));

    let result = console_error_pages(&con, &scan).expect("read console pages");

    assert_eq!(result["captured"], json!(2));
    assert_eq!(result["unreadable"], json!(0));
    assert_eq!(
        result["pages"],
        json!([{
            "target_url": "https://example.com/a",
            "error_count": 1,
            "errors": ["Uncaught TypeError: x"],
        }])
    );
}

#[test]
fn omitted_console_errors_still_make_a_page_a_finding() {
    let dir = TmpDir::new("omitted");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({"ok": true, "console_errors": [], "console_errors_omitted": 4}),
    );

    let result = console_error_pages(&con, &scan).expect("read console pages");
    let pages = result["pages"].as_array().expect("pages array");

    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["error_count"], json!(4));
    assert_eq!(pages[0]["errors"], json!([]));
}

#[test]
fn missing_or_tampered_sidecar_is_unreadable_not_clean() {
    let dir = TmpDir::new("tampered");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({"ok": true, "console_errors": ["boom"]}),
    );
    let payload = only_payload(&con);
    std::fs::remove_file(sidecar_path(&scan, &payload)).expect("remove sidecar");

    let result = console_error_pages(&con, &scan).expect("read console pages");

    assert_eq!(
        serde_json::to_value(&result).expect("result is JSON"),
        json!({"pages": [], "captured": 0, "unreadable": 1})
    );
}

#[test]
fn console_not_retained_yields_no_evidence() {
    let dir = TmpDir::new("not-retained");
    let scan = dir.scan();
    let con = connection();
    store(&con, &scan, 1, 2, json!({"ok": false}));

    let result = console_error_pages(&con, &scan).expect("read console pages");

    assert_eq!(result["pages"], json!([]));
    assert_eq!(result["captured"], json!(0));
    assert_eq!(result["unreadable"], json!(0));
}

#[test]
fn console_messages_are_retained_alongside_errors() {
    let dir = TmpDir::new("messages");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({
            "ok": true,
            "console_errors": ["boom"],
            "console_messages": [
                message("error", "boom", json!({"url": "/assets/app.js", "line": 3, "column": 9})),
                message("log", "visit https://example.com/x?token=secret-y", json!({})),
                // A location that still carries a full URL is redacted again here.
                message("warning", "late", json!({"url": "https://example.com/late.js", "line": 8})),
            ],
            "console_messages_omitted": 0,
        }),
    );

    let body = sidecar_body(&scan, &only_payload(&con));

    assert_eq!(body["schema_version"], json!("browser_console.v1"));
    assert_eq!(body["errors"], json!(["boom"]));
    assert_eq!(
        body["messages"],
        json!([
            {
                "type": "error",
                "text": "boom",
                "location": {"url": "/assets/app.js", "line": 3, "column": 9},
            },
            {"type": "log", "text": "visit [url]", "location": {}},
            {"type": "warning", "text": "late", "location": {"url": "[url]", "line": 8}},
        ])
    );
    assert_eq!(body["messages_truncated"], json!(false));
    // The pre-#1475 errors reader keeps working with messages present.
    let result = console_error_pages(&con, &scan).expect("read console pages");
    let pages = result["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["error_count"], json!(1));
}

#[test]
fn console_message_truncation_is_recorded_in_the_sidecar() {
    let dir = TmpDir::new("truncated");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({
            "ok": true,
            "console_errors": [],
            "console_messages": [],
            "console_messages_omitted": 7,
        }),
    );

    let body = sidecar_body(&scan, &only_payload(&con));

    assert_eq!(body["messages"], json!([]));
    assert_eq!(body["messages_truncated"], json!(true));
}

#[test]
fn sidecars_from_before_messages_still_store_and_read() {
    let dir = TmpDir::new("legacy");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({"ok": true, "console_errors": ["boom"]}),
    );

    let payload = only_payload(&con);
    assert!(sidecar_body(&scan, &payload).get("messages").is_none());
    let result = retained_console(&con, &scan, 1).expect("read retained console");
    assert_eq!(
        serde_json::to_value(&result).expect("result is JSON"),
        json!({
            "state": "stored",
            "reason": "",
            "document_id": 2,
            "messages": [],
            "messages_truncated": false,
        })
    );
}

#[test]
fn retained_console_reads_the_latest_render_only() {
    let dir = TmpDir::new("latest");
    let scan = dir.scan();
    let con = connection();
    con.execute("INSERT INTO documents VALUES(6, 1)", [])
        .expect("insert document");
    store(
        &con,
        &scan,
        1,
        2,
        json!({
            "ok": true,
            "console_errors": ["old"],
            "console_messages": [message("log", "old probe", json!({}))],
        }),
    );
    store(
        &con,
        &scan,
        1,
        6,
        json!({
            "ok": true,
            "console_errors": ["new"],
            "console_messages": [
                message("error", "new", json!({"url": "/n.js", "line": 1, "column": 2})),
            ],
        }),
    );
    // Page 3 has its own record that must stay invisible to page 1.
    store(
        &con,
        &scan,
        3,
        4,
        json!({
            "ok": true,
            "console_errors": [],
            "console_messages": [message("log", "other page", json!({}))],
            "console_messages_omitted": 3,
        }),
    );

    let result = retained_console(&con, &scan, 1).expect("read retained console");

    assert_eq!(
        serde_json::to_value(&result).expect("result is JSON"),
        json!({
            "state": "stored",
            "reason": "",
            "document_id": 6,
            "messages": [{
                "type": "error",
                "text": "new",
                "location": {"url": "/n.js", "line": 1, "column": 2},
            }],
            "messages_truncated": false,
        })
    );
}

#[test]
fn retained_console_names_a_page_without_artifacts() {
    let dir = TmpDir::new("no-artifacts");
    let scan = dir.scan();
    let con = connection();

    let result = retained_console(&con, &scan, 1).expect("read retained console");

    assert_eq!(
        serde_json::to_value(&result).expect("result is JSON"),
        json!({
            "state": "unavailable",
            "reason": "no rendered browser artifact was retained for this URL",
            "document_id": null,
            "messages": [],
            "messages_truncated": false,
        })
    );
}

#[test]
fn retained_console_survives_a_tampered_sidecar() {
    let dir = TmpDir::new("tampered-console");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({
            "ok": true,
            "console_errors": ["boom"],
            "console_messages": [message("error", "boom", json!({}))],
        }),
    );
    std::fs::remove_file(sidecar_path(&scan, &only_payload(&con))).expect("remove sidecar");

    let result = retained_console(&con, &scan, 1).expect("read retained console");

    assert_eq!(result["state"], json!("unavailable"));
    assert_eq!(result["document_id"], json!(2));
    assert_eq!(result["messages"], json!([]));
}

#[test]
fn retained_console_reports_the_disabled_state_without_a_read() {
    let dir = TmpDir::new("disabled");
    let scan = dir.scan();
    let con = connection();
    store(
        &con,
        &scan,
        1,
        2,
        json!({
            "ok": true,
            "console_errors": [],
            "console_messages": [message("log", "x", json!({}))],
        }),
    );
    let mut payload = only_payload(&con);
    payload["console"] =
        json!({"state": "disabled", "reason": "console retention disabled", "ref": null});
    con.execute(
        "UPDATE context_items SET payload_json=?1 WHERE kind='browser_artifacts'",
        params![payload.to_string()],
    )
    .expect("update payload");

    let result = retained_console(&con, &scan, 1).expect("read retained console");

    assert_eq!(result["state"], json!("disabled"));
    assert_eq!(result["messages"], json!([]));
}

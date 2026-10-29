//!
//! The reference `tmp_path` becomes a path under the system temp directory. The scenario writes no
//! sidecar: the render did not complete, so console evidence is unavailable and screenshots are
//! disabled.

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_store::browser_artifacts::{save, validate_context};
use std::path::PathBuf;

fn connection() -> Connection {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE pages(url_id INTEGER PRIMARY KEY);
         CREATE TABLE documents(document_id INTEGER PRIMARY KEY, url_id INTEGER);
         INSERT INTO pages VALUES(1);
         INSERT INTO documents VALUES(2,1);",
    )
    .unwrap();
    con
}

#[test]
fn browser_artifact_context_is_document_scoped_and_failed_console_is_unavailable() {
    let con = connection();
    let scan_path: PathBuf = std::env::temp_dir()
        .join("spiderhead-browser-artifacts-port")
        .join("scan.sqlite");
    let item = save(&scan_path, 1, 2, &json!({"ok": false}), false, true).unwrap();
    let payload: Value = serde_json::from_str(item["payload_json"].as_str().unwrap()).unwrap();

    assert_eq!(item["item_key"], json!("page:1:document:2"));
    assert_eq!(
        payload["console"],
        json!({
            "state": "unavailable",
            "reason": "render did not complete; console evidence is unavailable",
            "ref": null,
        })
    );
    assert_eq!(item["completeness"], json!("unavailable"));
    validate_context(&con, &item, &payload).unwrap();
}

//! The two lock-backend cases monkeypatch `filesystem.fcntl`/`msvcrt` and
//! `filesystem.lock_exclusive` — reference-only mechanics with no Rust seam; the
//! busy-writer case is exercised here against real `flock` contention.

use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_projects::action_log;
use spiderhead_projects::permissions::require_mcp_allowed;
use spiderhead_projects::runtime::project_write_lock;
use spiderhead_projects::workspace::create_project;

/// Per-test scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The `project` the reference suite fixture.
fn project(name: &str) -> (Scratch, PathBuf) {
    let dir = std::env::temp_dir().join(format!("sh-proj-alog-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("site");
    create_project(&root, "https://example.test/", None, None, None, None).unwrap();
    (Scratch(dir), root)
}

#[test]
fn action_log_filters_by_client_outcome_and_text() {
    let (_dir, root) = project("filters");
    action_log::record(
        &root,
        "mcp",
        "project.settings.propose",
        "pending",
        "Proposed a label change",
        Some("abc123"),
        None,
    )
    .unwrap();
    action_log::record(
        &root,
        "cli",
        "project.settings.review",
        "denied",
        "Owner denied the proposal",
        Some("abc123"),
        None,
    )
    .unwrap();

    let pending = action_log::page(&root, 0, 50, Some("mcp"), Some("pending"), "label").unwrap();
    let denied = action_log::page(&root, 0, 50, Some("cli"), Some("denied"), "").unwrap();

    assert_eq!(pending["revision"], json!(2));
    assert_eq!(pending["items"][0]["proposal_id"], json!("abc123"));
    assert_eq!(denied["items"][0]["client"], json!("cli"));
}

#[test]
fn owner_only_actions_fail_closed_over_mcp() {
    let err = require_mcp_allowed("project.settings.review").unwrap_err();
    assert!(
        err.to_string().contains("no trusted caller identity"),
        "unexpected: {err}"
    );
    require_mcp_allowed("project.settings.propose").unwrap();
}

#[test]
fn action_log_fails_fast_when_another_writer_holds_the_lock() {
    let (_dir, root) = project("busy");
    let _guard = project_write_lock(&root, 5.0).unwrap();
    // `project_write_lock` is re-entrant per thread, so contention must come
    // from a second thread — matching the monkeypatched `lock_exclusive` raise.
    let other = {
        let root = root.clone();
        std::thread::spawn(move || project_write_lock(&root, 0.0))
    };
    let err = match other.join().unwrap() {
        Err(e) => e,
        Ok(_lock) => panic!("a second writer must not take the held project lock"),
    };
    assert!(
        err.to_string().contains("project workspace is busy"),
        "unexpected: {err}"
    );
}

#[test]
fn pending_action_resolves_in_place_without_a_second_slot() {
    let (_dir, root) = project("resolve");
    let pending = action_log::record(
        &root,
        "mcp",
        "project.settings.propose",
        "pending",
        "A long pending detail reserves the terminal result row.",
        Some("abc123"),
        None,
    )
    .unwrap()["action"]
        .clone();
    let resolved = action_log::resolve_pending(
        &root,
        pending["sequence"].as_i64().unwrap(),
        "applied",
        "Approved.",
        Some("abc123"),
        None,
    )
    .unwrap();
    assert_eq!(resolved["action"]["sequence"], pending["sequence"]);
    assert_eq!(resolved["action"]["outcome"], json!("applied"));
    // reference pins MAX_BYTES to prove the row is replaced in place, never appended.
    let lines = std::fs::read_to_string(root.join(action_log::FILE))
        .unwrap()
        .lines()
        .count();
    assert_eq!(lines, 1);
}

#[test]
fn page_does_not_create_or_change_project_files() {
    let (_dir, root) = project("notouch");
    let snapshot = |root: &std::path::Path| -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                out.push((
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
        out.sort();
        out
    };
    let before = snapshot(&root);
    assert_eq!(
        action_log::page(&root, 0, 50, None, None, "").unwrap()["items"],
        json!([])
    );
    assert_eq!(snapshot(&root), before);
}

#[test]
fn page_normalizes_legacy_rows_without_rewriting_them() {
    let (_dir, root) = project("legacy");
    let path = root.join(action_log::FILE);
    let row = json!({
        "sequence": 1,
        "occurred_at": "2026-10-11T10:00:00Z",
        "client": "cli",
        "action": "project.settings.update",
        "outcome": "applied",
        "detail": "Legacy action",
        "proposal_id": null,
    });
    std::fs::write(&path, format!("{row}\n")).unwrap();
    let original = std::fs::read(&path).unwrap();

    let result = action_log::page(&root, 0, 50, None, None, "").unwrap();
    assert_eq!(result["items"][0]["operation_id"], Value::Null);
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

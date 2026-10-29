//! conditional reads for issue #979/#953. The CLI flag-mapping case belongs
//! to the cli slice.

#[path = "common/testkit.rs"]
mod testkit;

use std::io::Write;

use spiderhead_projects::observer::{observe, observe_activity};

#[test]
fn project_activity_returns_small_not_modified_response() {
    let (_dir, root) = testkit::project("obsrev", "activity");

    let first = observe_activity(&root, None).expect("observe_activity");
    let revision = first["revision"].as_str().expect("revision").to_owned();
    let unchanged = observe_activity(&root, Some(&revision)).expect("observe_activity");

    assert_eq!(revision.len(), 64);
    assert_eq!(
        unchanged,
        serde_json::json!({"ok": true, "revision": revision, "not_modified": true})
    );
}

#[test]
fn project_observe_revision_changes_with_saved_snapshot_data() {
    let (_dir, root) = testkit::project("obsrev", "observe");

    let first = observe(&root, None, 1, 0, 20, None).expect("observe");
    let revision = first["revision"].as_str().expect("revision").to_owned();
    let unchanged = observe(&root, None, 1, 0, 20, Some(&revision)).expect("observe");
    {
        let mut stream = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("log.md"))
            .expect("log.md");
        stream
            .write_all(b"\nA recorded synthetic change.\n")
            .expect("append");
    }
    let changed = observe(&root, None, 1, 0, 20, Some(&revision)).expect("observe");

    assert_eq!(unchanged["not_modified"], true);
    assert_eq!(changed["not_modified"], false);
    assert_ne!(changed["revision"], revision);
}

#[test]
fn conditional_revision_is_a_validated_sha256_token() {
    let (_dir, root) = testkit::project("obsrev", "invalid");

    let err = observe_activity(&root, Some("short")).expect_err("must reject");
    assert!(err.to_string().contains("64-character"), "got: {err}");
}

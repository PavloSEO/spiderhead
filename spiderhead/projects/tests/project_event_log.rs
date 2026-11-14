//! append/read contract. The shared-registry handler case and the CLI
//! `_build_kwargs` case belong to the mcp/cli slices, and the Windows lock
//! backend case monkeypatches `fcntl`/`msvcrt` — reference-only mechanics.

use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_projects::event_log;
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
    let dir = std::env::temp_dir().join(format!("sh-proj-elog-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("site");
    create_project(&root, "https://example.test/", None, None, None, None).unwrap();
    (Scratch(dir), root)
}

fn add(project: &std::path::Path, source: &str, actor: &str, text: &str) -> Value {
    event_log::append(project, source, actor, text).unwrap()["event"].clone()
}

#[test]
fn append_then_read_newest_first_with_source_actor_and_sequence() {
    let (_dir, root) = project("order");
    let first = add(&root, "user", "user", "Pause the crawl");
    let second = add(&root, "scans", "schedule", "Scan 3 finished");
    let result = event_log::page(&root, 0, 50, None, "", None, false, 0).unwrap();
    assert_eq!(result["total"], json!(2));
    let seqs: Vec<i64> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["sequence"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, [2, 1]);
    assert_eq!(result["items"][0], second);
    assert_eq!(result["items"][1], first);
    assert_eq!(result["items"][0]["source"], json!("scans"));
    assert_eq!(result["items"][0]["actor"], json!("schedule"));
}

#[test]
fn append_never_touches_log_md_or_project_json() {
    let (_dir, root) = project("notouch");
    let before: Vec<(String, Vec<u8>)> = ["log.md", "project.json"]
        .iter()
        .map(|n| (n.to_string(), std::fs::read(root.join(n)).unwrap()))
        .collect();
    add(&root, "agent", "agent", "Note only");
    for (name, bytes) in &before {
        assert_eq!(&std::fs::read(root.join(name)).unwrap(), bytes);
    }
    assert!(root.join(event_log::FILE).is_file());
}

#[test]
fn source_and_case_insensitive_text_filters() {
    let (_dir, root) = project("filters");
    add(&root, "agent", "agent", "Found 52 new URLs");
    add(&root, "user", "user", "Check the robots rules");
    add(&root, "agent", "agent", "found a redirect loop");

    let agent_only = event_log::page(&root, 0, 50, Some("agent"), "", None, false, 0).unwrap();
    assert_eq!(agent_only["total"], json!(2));
    assert!(agent_only["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["source"] == "agent"));

    let matched = event_log::page(&root, 0, 50, None, "FOUND", None, false, 0).unwrap();
    assert_eq!(matched["total"], json!(2));

    let combined = event_log::page(&root, 0, 50, Some("user"), "robots", None, false, 0).unwrap();
    assert_eq!(combined["total"], json!(1));
    assert_eq!(
        combined["items"][0]["text"],
        json!("Check the robots rules")
    );
}

#[test]
fn revision_cursor_returns_only_newer_events_and_marks_unchanged() {
    let (_dir, root) = project("cursor");
    let first = add(&root, "agent", "agent", "first");
    add(&root, "agent", "agent", "second");

    let changed = event_log::page(
        &root,
        0,
        50,
        None,
        "",
        Some(first["sequence"].as_i64().unwrap()),
        false,
        0,
    )
    .unwrap();
    let unchanged = event_log::page(&root, 0, 50, None, "", Some(2), false, 0).unwrap();

    assert_eq!(changed["revision"], json!(2));
    assert_eq!(changed["not_modified"], json!(false));
    let texts: Vec<&str> = changed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["second"]);
    assert_eq!(unchanged["revision"], json!(2));
    assert_eq!(unchanged["not_modified"], json!(true));
    assert_eq!(unchanged["items"], json!([]));
}

#[test]
fn follow_waits_for_a_new_event() {
    let (_dir, root) = project("follow");
    add(&root, "agent", "agent", "first");
    // reference monkeypatches `event_log.time.sleep`; a real concurrent appender
    // exercises the same follow loop against the real clock.
    let writer = {
        let root = root.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            add(&root, "agent", "agent", "followed");
        })
    };
    let result = event_log::page(&root, 0, 50, None, "", Some(1), true, 1).unwrap();
    writer.join().unwrap();
    assert_eq!(result["revision"], json!(2));
    assert_eq!(result["not_modified"], json!(false));
    let texts: Vec<&str> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["followed"]);
}

#[test]
fn pagination_walks_all_events_without_gaps() {
    let (_dir, root) = project("pages");
    for index in 0..5 {
        add(&root, "agent", "agent", &format!("event {index}"));
    }
    let mut seen = Vec::new();
    let mut offset = Some(0i64);
    while let Some(o) = offset {
        let result = event_log::page(&root, o, 2, None, "", None, false, 0).unwrap();
        for row in result["items"].as_array().unwrap() {
            seen.push(row["text"].as_str().unwrap().to_owned());
        }
        offset = result["pagination"]["next_offset"].as_i64();
    }
    let want: Vec<String> = (0..5).rev().map(|i| format!("event {i}")).collect();
    assert_eq!(seen, want);
    let last =
        event_log::page(&root, 4, 2, None, "", None, false, 0).unwrap()["pagination"].clone();
    assert_eq!(last["next_offset"], Value::Null);
    assert_eq!(last["previous_offset"], json!(2));
}

#[test]
fn empty_log_reads_as_empty_page() {
    let (_dir, root) = project("empty");
    let result = event_log::page(&root, 0, 50, None, "", None, false, 0).unwrap();
    assert_eq!(result["total"], json!(0));
    assert_eq!(result["items"], json!([]));
}

#[test]
fn invalid_appends_are_refused_and_write_nothing() {
    let (_dir, root) = project("badappend");
    for (source, actor, text) in [
        ("bot", "agent", "x"),
        ("agent", "robot", "x"),
        ("agent", "agent", "   "),
        ("agent", "agent", "a\0b"),
    ] {
        assert!(
            event_log::append(&root, source, actor, text).is_err(),
            "{source}/{actor}/{text:?} should be refused"
        );
    }
    let too_long = "x".repeat(event_log::MAX_TEXT + 1);
    assert!(event_log::append(&root, "agent", "agent", &too_long).is_err());
    assert!(!root.join(event_log::FILE).exists());
}

#[test]
fn invalid_reads_are_refused() {
    let (_dir, root) = project("badreads");
    assert!(event_log::page(&root, -1, 50, None, "", None, false, 0).is_err());
    assert!(event_log::page(&root, 0, 0, None, "", None, false, 0).is_err());
    assert!(event_log::page(&root, 0, event_log::MAX_LIMIT + 1, None, "", None, false, 0).is_err());
    assert!(event_log::page(&root, 0, 50, Some("bot"), "", None, false, 0).is_err());
    let long_query = "x".repeat(event_log::MAX_QUERY + 1);
    assert!(event_log::page(&root, 0, 50, None, &long_query, None, false, 0).is_err());
    assert!(event_log::page(&root, 0, 50, None, "", Some(-1), false, 0).is_err());
    assert!(event_log::page(&root, 0, 50, None, "", None, true, 0).is_err());
    assert!(event_log::page(
        &root,
        0,
        50,
        None,
        "",
        Some(0),
        true,
        event_log::MAX_FOLLOW_SECONDS + 1
    )
    .is_err());
}

#[test]
fn malformed_or_unsafe_event_file_refuses_instead_of_hiding_rows() {
    let (_dir, root) = project("malformed");
    add(&root, "agent", "agent", "good");
    let path = root.join(event_log::FILE);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{not json}\n")
        .unwrap();
    let err = event_log::page(&root, 0, 50, None, "", None, false, 0).unwrap_err();
    assert!(err.to_string().contains("line 2"), "unexpected: {err}");

    std::fs::remove_file(&path).unwrap();
    let row = json!({
        "sequence": 1,
        "occurred_at": "2026-10-10T00:00:00Z",
        "source": "app",
        "actor": "agent",
        "text": "x",
        "extra": 1,
    });
    std::fs::write(&path, format!("{row}\n")).unwrap();
    assert!(event_log::page(&root, 0, 50, None, "", None, false, 0).is_err());
}

#[test]
#[cfg(unix)]
fn symlinked_event_file_refuses() {
    use std::io::Write;
    let (dir, root) = project("symlink");
    let target = dir.0.join("elsewhere.jsonl");
    std::fs::File::create(&target)
        .unwrap()
        .write_all(b"")
        .unwrap();
    std::os::unix::fs::symlink(&target, root.join(event_log::FILE)).unwrap();
    let err = event_log::append(&root, "agent", "agent", "x").unwrap_err();
    assert!(err.to_string().contains("unsafe"), "unexpected: {err}");
    let err = event_log::page(&root, 0, 50, None, "", None, false, 0).unwrap_err();
    assert!(err.to_string().contains("unsafe"), "unexpected: {err}");
}

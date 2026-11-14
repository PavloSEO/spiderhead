//! `build_server` mcp-server cases, the `spawn` process-boundary cases and
//! the `msvcrt` lock-backend case stay deferred (mcp slice / reference-only
//! seams). The separate-process handoff is approximated with a second
//! writer thread — the durability claim under test (a foreign write visible
//! to a fresh consumer) is identical. `_finished` scan writes become the
//! bundled finished-scan fixture.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::coverage::{coverage_status, initialize_coverage, update_item};
use spiderhead_projects::inbox::{
    acknowledge, fingerprint, goal_task_id, list_entries, mark_read, set_goal_state, submit,
    triage, unread_goal_tasks, unread_summary,
};
use spiderhead_projects::observer;
use spiderhead_projects::workspace::create_project;

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("synthetic-project");
    create_project(
        &root,
        "https://example.test/",
        Some("Synthetic observer fixture"),
        None,
        None,
        None,
    )
    .expect("create");
    root
}

fn files(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    for entry in walkdir(root) {
        if entry.is_file() {
            let rel = entry
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            out.insert(rel, std::fs::read(&entry).expect("read"));
        }
    }
    out
}

fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for e in read.flatten() {
                let p = e.path();
                if p.is_dir() && !p.is_symlink() {
                    stack.push(p);
                } else {
                    paths.push(p);
                }
            }
        }
    }
    paths.sort();
    paths
}

#[test]
fn note_read_ack_and_goal_transitions_are_explicit_and_durable() {
    let dir = testkit::scratch("inbox", "transitions");
    let root = project(&dir);
    let note = submit(
        &root,
        "Review sitemap gap",
        "note",
        Some(&json!(["scan:synthetic-1"])),
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    let goal = submit(
        &root,
        "Compare the named competitor",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();

    assert_eq!(
        unread_summary(&root, "agent/session-a", 20).expect("unread")["count"],
        2
    );
    let inspected =
        mark_read(&root, "agent/session-a", &json!([note["id"].clone()]), None).expect("mark_read");
    assert!(inspected["entries"][0]["read_at"].is_string());
    assert!(inspected["entries"][0]["acknowledged_at"].is_null());
    assert_eq!(
        unread_summary(&root, "agent/session-a", 20).expect("unread")["count"],
        2
    );
    let acknowledged =
        acknowledge(&root, "agent/session-a", &json!([note["id"].clone()]), None).expect("ack");
    assert!(acknowledged["entries"][0]["acknowledged_at"].is_string());
    assert_eq!(
        unread_summary(&root, "agent/session-a", 20).expect("unread")["count"],
        1
    );
    assert_eq!(
        set_goal_state(&root, goal["id"].as_str().unwrap(), "accepted", None).expect("accept")
            ["entry"]["goal_state"],
        "accepted"
    );
    assert_eq!(
        set_goal_state(&root, goal["id"].as_str().unwrap(), "completed", None).expect("complete")
            ["entry"]["goal_state"],
        "completed"
    );
    let err = set_goal_state(&root, note["id"].as_str().unwrap(), "accepted", None)
        .expect_err("note has no goal");
    assert!(err.to_string().contains("proposed goal"), "got: {err}");
}

#[test]
fn goal_transitions_record_timestamps_once() {
    let dir = testkit::scratch("inbox", "timestamps");
    let root = project(&dir);
    let goal = submit(
        &root,
        "Ship the audit",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    assert!(goal["accepted_at"].is_null() && goal["completed_at"].is_null());
    let gid = goal["id"].as_str().unwrap();
    let accepted = set_goal_state(&root, gid, "accepted", None).expect("accept")["entry"].clone();
    assert!(accepted["accepted_at"].is_string() && accepted["completed_at"].is_null());
    let again = set_goal_state(&root, gid, "accepted", None).expect("reaccept")["entry"].clone();
    assert_eq!(again["accepted_at"], accepted["accepted_at"]);
    let completed =
        set_goal_state(&root, gid, "completed", None).expect("complete")["entry"].clone();
    assert!(completed["completed_at"].is_string());
    assert_eq!(completed["accepted_at"], accepted["accepted_at"]);
}

#[test]
fn specialist_note_triage_links_existing_tasks_goals_and_competitor_suggestions() {
    let dir = testkit::scratch("inbox", "triage-links");
    let root = project(&dir);
    let initialized = initialize_coverage(&root, None).expect("coverage");
    let task_id = "custom:review-competitor";
    update_item(
        &root,
        &json!({"id": task_id, "title": "Review named competitor"}),
        initialized["revision"].as_i64().unwrap(),
    )
    .expect("update_item");
    let task_note = submit(
        &root,
        "Please compare the named competitor",
        "note",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    let task = triage(
        &root,
        task_note["id"].as_str().unwrap(),
        &json!({"kind": "task", "reason": "The controller created a scoped review task before work.", "task_ids": [task_id]}),
        "agent/controller",
        None,
    )
    .expect("triage")["entry"].clone();
    assert_eq!(
        task["triage"].as_array().unwrap().last().unwrap()["task_ids"],
        json!([task_id])
    );
    assert_eq!(
        unread_summary(&root, "agent/controller", 20).expect("unread")["count"],
        1
    );

    let goal = submit(
        &root,
        "Decide the next audit scope",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("goal")["entry"]
        .clone();
    let goal_note = submit(
        &root,
        "Turn this into a proposed audit goal",
        "note",
        None,
        "specialist",
        None,
    )
    .expect("note")["entry"]
        .clone();
    let linked_goal = triage(
        &root,
        goal_note["id"].as_str().unwrap(),
        &json!({"kind": "goal", "reason": "The specialist must still explicitly accept the proposed goal.", "goal_id": goal["id"]}),
        "agent/controller",
        None,
    )
    .expect("triage")["entry"].clone();
    let last = linked_goal["triage"].as_array().unwrap().last().unwrap();
    assert_eq!(last["goal_id"], goal["id"]);
    assert_eq!(last["kind"], "goal");

    let competitor_note = submit(
        &root,
        "Consider this competitor",
        "note",
        None,
        "specialist",
        None,
    )
    .expect("note")["entry"]
        .clone();
    let candidate = triage(
        &root,
        competitor_note["id"].as_str().unwrap(),
        &json!({"kind": "competitor", "reason": "Candidate retained for later explicit project preparation.", "competitors": ["https://competitor.test/"]}),
        "agent/controller",
        None,
    )
    .expect("triage")["entry"].clone();
    assert_eq!(
        candidate["triage"].as_array().unwrap().last().unwrap()["competitors"],
        json!(["https://competitor.test/"])
    );
    assert_eq!(
        coverage_status(&root, false).expect("status")["state"],
        "initialized"
    );
}

#[test]
fn specialist_note_triage_requires_explicit_nonexecution_reason() {
    for kind in ["blocked", "rejected"] {
        let dir = testkit::scratch("inbox", &format!("triage-{kind}"));
        let root = project(&dir);
        let note = submit(
            &root,
            "A question requiring a decision",
            "note",
            None,
            "specialist",
            None,
        )
        .expect("submit")["entry"]
            .clone();
        let result = triage(
            &root,
            note["id"].as_str().unwrap(),
            &json!({"kind": kind, "reason": "Synthetic missing evidence."}),
            "agent/controller",
            None,
        )
        .expect("triage");
        let last = result["entry"]["triage"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last["kind"], kind);
        assert_eq!(last["reason"], "Synthetic missing evidence.");
    }
}

#[test]
fn specialist_note_triage_does_not_link_a_completed_goal() {
    let dir = testkit::scratch("inbox", "triage-completed");
    let root = project(&dir);
    let goal = submit(
        &root,
        "Completed work",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("goal")["entry"]
        .clone();
    let gid = goal["id"].as_str().unwrap();
    set_goal_state(&root, gid, "accepted", None).expect("accept");
    set_goal_state(&root, gid, "completed", None).expect("complete");
    let note = submit(
        &root,
        "Reuse the completed goal",
        "note",
        None,
        "specialist",
        None,
    )
    .expect("note")["entry"]
        .clone();
    let err = triage(
        &root,
        note["id"].as_str().unwrap(),
        &json!({"kind": "goal", "reason": "This must fail because the target is already complete.", "goal_id": gid}),
        "agent/controller",
        None,
    )
    .expect_err("completed goal");
    assert!(
        err.to_string().contains("current stored proposed goal"),
        "got: {err}"
    );
}

#[test]
fn concurrent_observer_submissions_are_not_lost_and_read_only_is_stable() {
    let dir = testkit::scratch("inbox", "concurrent");
    let root = project(&dir);
    let scan_hash = fingerprint(&root).expect("fingerprint");
    let shared = std::sync::Arc::new(root.clone());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for i in 0..8 {
        let r = std::sync::Arc::clone(&shared);
        let b = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            b.wait();
            submit(
                &r,
                &format!("Synthetic note {i}"),
                "note",
                Some(&json!(["section:coverage"])),
                "specialist",
                None,
            )
            .expect("submit")["entry"]["id"]
                .as_str()
                .unwrap()
                .to_string()
        }));
    }
    let ids: std::collections::HashSet<String> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(root.join(".inbox.lock").is_file());
    let listed = list_entries(&root, "agent/session-a", 0, 20, false).expect("list");
    let listed_ids: std::collections::HashSet<String> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed_ids, ids);
    assert_eq!(listed["pagination"]["total"], 8);
    assert_ne!(fingerprint(&root).expect("fp"), scan_hash);
    let again = fingerprint(&root).expect("fp");
    unread_summary(&root, "agent/session-a", 20).expect("unread");
    assert_eq!(fingerprint(&root).expect("fp"), again);
}

#[test]
fn inbox_reads_and_observer_snapshot_leave_a_new_project_byte_identical() {
    let dir = testkit::scratch("inbox", "byte-identical");
    let root = project(&dir);
    let before = files(&root);
    assert_eq!(
        list_entries(&root, "agent/session-a", 0, 20, false).expect("list")["entries"],
        json!([])
    );
    assert_eq!(
        unread_summary(&root, "agent/session-a", 20).expect("unread")["count"],
        0
    );
    assert_eq!(
        observer::observe(&root, Some("agent/session-a"), 20, 0, 20, None).expect("observe")
            ["inbox_unread"]["count"],
        0
    );
    assert_eq!(files(&root), before);
    assert!(!root.join(".inbox.lock").exists());
}

// (second writer thread stands in for the spawned writer process)
#[test]
fn separate_process_handoff_survives_writer_exit() {
    let dir = testkit::scratch("inbox", "handoff");
    let root = project(&dir);
    let r = root.clone();
    std::thread::spawn(move || {
        submit(
            &r,
            "Written by a separate collector",
            "note",
            Some(&json!(["scan:synthetic-process"])),
            "specialist",
            None,
        )
        .expect("writer submit");
    })
    .join()
    .expect("writer exit");
    let handed_off = list_entries(&root, "agent/replacement", 0, 20, false).expect("list");
    assert_eq!(
        handed_off["entries"][0]["text"],
        "Written by a separate collector"
    );
    assert_eq!(
        unread_summary(&root, "agent/replacement", 20).expect("unread")["count"],
        1
    );
}

#[test]
fn observer_snapshot_keeps_missing_work_and_logs_visible() {
    let dir = testkit::scratch("inbox", "observer-snapshot");
    let root = project(&dir);
    std::fs::write(
        root.join("log.md"),
        "# Project log\n\nSynthetic handoff evidence\n",
    )
    .expect("log");
    submit(
        &root,
        "Review missing competitor",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("goal");
    let result =
        observer::observe(&root, Some("agent/session-a"), 20, 0, 20, None).expect("observe");
    assert_eq!(result["progress"]["audit_complete"], false);
    assert_eq!(result["preparation"]["state"], "pending");
    assert_eq!(result["execution"]["runs"], json!([]));
    assert!(result["monitor"]["policy"].is_null() && result["monitor"]["last_run"].is_null());
    assert_eq!(result["scans"]["total"], 0);
    assert_eq!(result["inbox_unread"]["count"], 1);
    assert!(result["log"]["text"]
        .as_str()
        .unwrap()
        .contains("Synthetic handoff evidence"));
}

#[test]
fn utf8_dictation_text_survives_handoff_and_restart() {
    let dir = testkit::scratch("inbox", "utf8");
    let root = project(&dir);
    let text = "Проверь, почему сценарий и конкурент ещё не пройдены";
    let entry = submit(&root, text, "proposed_goal", None, "specialist", None).expect("submit")
        ["entry"]
        .clone();
    let restored = list_entries(&root, "agent/next-session", 0, 20, false).expect("list")
        ["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(restored[0]["id"], entry["id"]);
    assert_eq!(restored[0]["text"], text);
}

#[test]
fn contextual_task_note_keeps_actual_identifier_through_durable_receipts() {
    for item_id in [
        "custom:late",
        "check:TITLE_MISSING",
        "scenario:full-audit",
        "skill:workflow/control",
        "skill:general/seo-content",
        "site:12345678-1234-4234-8234-123456789abc/custom:late",
        &format!("custom:{}", "a".repeat(128)),
        "synthetic",
    ] {
        let dir = testkit::scratch("inbox", "task-ref");
        let root = project(&dir);
        let reference = format!("task:{item_id}");
        let submitted = submit(
            &root,
            "Review this exact task",
            "note",
            Some(&json!([reference.clone()])),
            "specialist",
            None,
        )
        .expect("submit")["entry"]
            .clone();
        let entries = list_entries(&root, "agent/test", 0, 20, false).expect("list");
        assert_eq!(
            entries["entries"][0]["references"],
            json!([reference.clone()])
        );
        mark_read(&root, "agent/test", &json!([submitted["id"].clone()]), None).expect("read");
        acknowledge(&root, "agent/test", &json!([submitted["id"].clone()]), None).expect("ack");
        assert_eq!(
            list_entries(&root, "agent/test", 0, 20, true).expect("list")["entries"][0]
                ["references"],
            json!([reference])
        );
    }
}

#[test]
fn task_note_namespace_does_not_admit_arbitrary_paths_or_other_namespace_uris() {
    for reference in [
        "task:custom:../secret".to_string(),
        "task:custom:a:b".to_string(),
        "task:https://example.test/".to_string(),
        "task:unknown:value".to_string(),
        "task:site:invalid/custom:late".to_string(),
        "task:site:12345678-1234-4234-8234-123456789abc/../../secret".to_string(),
        format!("task:custom:{}", "a".repeat(129)),
        "scan:custom:late".to_string(),
        "finding:https://example.test/".to_string(),
    ] {
        let dir = testkit::scratch("inbox", "bad-ref");
        let root = project(&dir);
        submit(&root, "Existing note", "note", None, "specialist", None).expect("submit");
        let before = files(&root);
        let err = submit(
            &root,
            "Rejected reference",
            "note",
            Some(&json!([reference])),
            "specialist",
            None,
        )
        .expect_err("bad reference");
        assert!(
            err.to_string().contains("invalid project reference"),
            "{reference}: {err}"
        );
        assert_eq!(files(&root), before, "{reference} mutated the store");
    }
}

#[test]
fn question_entry_is_stored_without_goal_state() {
    let dir = testkit::scratch("inbox", "question");
    let root = project(&dir);
    let question = submit(
        &root,
        "Why is the sitemap gap open?",
        "question",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    assert_eq!(question["kind"], "question");
    assert!(question["goal_state"].is_null());
    let err = set_goal_state(&root, question["id"].as_str().unwrap(), "accepted", None)
        .expect_err("question has no goal");
    assert!(err.to_string().contains("proposed goal"), "got: {err}");
    let err =
        submit(&root, "Unknown kind", "ticket", None, "specialist", None).expect_err("bad kind");
    assert!(err.to_string().contains("kind must be"), "got: {err}");
}

#[test]
fn accepted_goal_creates_one_checklist_task_and_keeps_it_on_retry() {
    let dir = testkit::scratch("inbox", "goal-task");
    let root = project(&dir);
    let goal = submit(
        &root,
        "  Compare   the named\ncompetitor ",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    let gid = goal["id"].as_str().unwrap();
    set_goal_state(&root, gid, "accepted", None).expect("accept");
    set_goal_state(&root, gid, "accepted", None).expect("reaccept");
    let task_id = goal_task_id(gid);
    let items = coverage_status(&root, false).expect("status")["items"]
        .as_array()
        .unwrap()
        .clone();
    let rows: Vec<&Value> = items
        .iter()
        .filter(|r| r["id"].as_str() == Some(task_id.as_str()))
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], "Owner goal: Compare the named competitor");
    assert_eq!(rows[0]["kind"], "custom");
    assert_eq!(rows[0]["complete"], false);
    set_goal_state(&root, gid, "completed", None).expect("complete");
    assert!(coverage_status(&root, false).expect("status")["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"].as_str() == Some(task_id.as_str())));
}

#[test]
fn goal_acceptance_is_refused_without_a_task_when_the_checklist_is_busy() {
    let dir = testkit::scratch("inbox", "busy-checklist");
    let root = project(&dir);
    let goal = submit(
        &root,
        "Blocked by a writer",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    std::fs::write(root.join(".coverage.lock"), "").expect("lock file");
    let err =
        set_goal_state(&root, goal["id"].as_str().unwrap(), "accepted", None).expect_err("busy");
    assert!(
        err.to_string().contains("coverage writer is busy"),
        "got: {err}"
    );
    std::fs::remove_file(root.join(".coverage.lock")).expect("unlink");
    assert_eq!(
        list_entries(&root, "agent/session-a", 0, 20, false).expect("list")["entries"][0]
            ["goal_state"],
        "proposed"
    );
}

// (inbox layer; the mcp seo_project_progress assertion half stays deferred)
#[test]
fn accepted_goal_shows_as_unread_owner_task_until_acknowledged() {
    let dir = testkit::scratch("inbox", "unread-owner");
    let root = project(&dir);
    let goal = submit(
        &root,
        "Check the pricing pages",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    let gid = goal["id"].as_str().unwrap();
    set_goal_state(&root, gid, "accepted", None).expect("accept");
    let unread = unread_goal_tasks(&root, "agent/session-a", 100).expect("unread");
    assert_eq!(unread["count"], 1);
    assert_eq!(unread["items"][0]["task_id"], goal_task_id(gid));
    acknowledge(&root, "agent/session-a", &json!([gid]), None).expect("ack");
    assert_eq!(
        unread_goal_tasks(&root, "agent/session-a", 100).expect("unread")["count"],
        0
    );
    assert_eq!(
        unread_goal_tasks(&root, "agent/session-b", 100).expect("unread")["count"],
        1
    );
}

#[test]
fn accepted_goal_adds_only_its_own_task_to_a_fresh_checklist() {
    let dir = testkit::scratch("inbox", "fresh-checklist");
    let root = project(&dir);
    let goal = submit(
        &root,
        "Compare two competitors",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    let gid = goal["id"].as_str().unwrap();
    set_goal_state(&root, gid, "accepted", None).expect("accept");
    let stored: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("coverage.json")).expect("coverage.json"),
    )
    .expect("coverage json");
    // coverage.json persists items keyed by id; coverage_status returns them
    // as a list. `set(stored)` on the reference side iterates the dict keys.
    let items_obj = stored["items"].as_object().expect("items object");
    let ids: std::collections::HashSet<&str> = items_obj.keys().map(String::as_str).collect();
    assert_eq!(ids, [goal_task_id(gid).as_str()].into_iter().collect());
    let builtin = spiderhead_projects::catalogue::load_catalogue()
        .expect("catalogue")
        .keys()
        .next()
        .unwrap()
        .clone();
    let revision = coverage_status(&root, false).expect("status")["revision"]
        .as_i64()
        .unwrap();
    update_item(&root, &json!({"id": builtin, "priority": "P2"}), revision).expect("update");
    assert!(
        coverage_status(&root, false).expect("status")["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"].as_str() == Some(builtin.as_str()))
            .unwrap()["priority"]
            == "P2"
    );
}

#[test]
fn busy_checklist_names_the_live_collector_instead_of_a_leftover_lock() {
    let dir = testkit::scratch("inbox", "busy-live");
    let root = project(&dir);
    let goal = submit(
        &root,
        "Blocked by a running crawl",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    std::fs::create_dir_all(root.join("scans")).expect("scans");
    let run = spiderhead_projects::run_observation::start(
        &root,
        "native",
        "spider",
        10,
        20_000,
        3_600,
        None,
        "synthetic",
        Some(root.join("scans").join("scan.sqlite").to_str().unwrap()),
        false,
        None,
        None,
        None,
        None,
    )
    .expect("start run");
    std::fs::write(root.join(".coverage.lock"), "").expect("lock file");
    let err =
        set_goal_state(&root, goal["id"].as_str().unwrap(), "accepted", None).expect_err("busy");
    let expected = format!(
        "coverage writer is busy: native run {}",
        run["id"].as_str().unwrap()
    );
    assert!(err.to_string().contains(&expected), "got: {err}");
    std::fs::remove_file(root.join(".coverage.lock")).expect("unlink");
}

#[test]
fn unexplained_writer_lock_is_reported_as_a_leftover() {
    let dir = testkit::scratch("inbox", "leftover-lock");
    let root = project(&dir);
    let goal = submit(
        &root,
        "Blocked by a stale file",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .expect("submit")["entry"]
        .clone();
    std::fs::write(root.join(".coverage.lock"), "").expect("lock file");
    let err = set_goal_state(&root, goal["id"].as_str().unwrap(), "accepted", None)
        .expect_err("leftover");
    let text = err.to_string();
    assert!(
        text.contains("no live holder is recorded") && text.contains("leftover"),
        "got: {text}"
    );
    std::fs::remove_file(root.join(".coverage.lock")).expect("unlink");
}

//! `project_write_lock`/`write_document` monkeypatches become real seams:
//! an OS lock held by a second thread, a directory swapped over the
//! proposals file (atomic rename fails), and pre-seeded `applying` proposal
//! documents for the crashed-finalize recoveries. `the reference suite.raises(match=)`
//! becomes error-message substring assertions.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::action_log::page as action_page;
use spiderhead_projects::locks;
use spiderhead_projects::project_settings::{
    delete_project, list_proposals, propose_settings, read_settings, review_proposal, update_label,
    PROPOSALS_FILE, PROPOSALS_FORMAT, SETTINGS_FILE, SETTINGS_FORMAT,
};
use spiderhead_projects::run_observation;
use spiderhead_projects::runtime::{default_policy, project_policy, POLICY_FORMAT};
use spiderhead_projects::workspace::create_project;

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("site");
    create_project(
        &root,
        "https://example.test/",
        Some("Example"),
        None,
        None,
        None,
    )
    .expect("create");
    root
}

fn project_uuid(root: &std::path::Path) -> String {
    serde_json::from_str::<Value>(
        &std::fs::read_to_string(root.join("project.json")).expect("project.json"),
    )
    .expect("project json")["project_uuid"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The `project_write_lock` contention monkeypatch: another thread owns the
/// OS lock while the caller retries past its bounded wait.
fn hold_write_lock(root: &std::path::Path) -> std::sync::mpsc::Receiver<()> {
    let path = root.to_path_buf();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let file = locks::open_lock(&path.join(".project-write.lock"), true).expect("open lock");
        locks::lock_exclusive(&file).expect("lock");
        held_tx.send(()).expect("signal");
        std::thread::sleep(std::time::Duration::from_secs(20));
    });
    held_rx
}

#[test]
fn project_label_update_requires_current_revision() {
    let dir = testkit::scratch("settings", "label");
    let root = project(&dir);
    assert_eq!(read_settings(&root).expect("read")["revision"], 0);
    let updated = update_label(&root, "Updated site", 0, "cli", None, true).expect("update");
    assert_eq!(updated["revision"], 1);
    assert_eq!(updated["settings"]["label"], "Updated site");
    let err = update_label(&root, "Stale", 0, "cli", None, true).expect_err("stale");
    assert!(err.to_string().contains("revision conflict"), "got: {err}");
}

#[test]
fn legacy_settings_operation_identity_defaults_without_rewriting() {
    let dir = testkit::scratch("settings", "legacy-settings");
    let root = project(&dir);
    let uuid = project_uuid(&root);
    let path = root.join(SETTINGS_FILE);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "format": SETTINGS_FORMAT,
            "version": 1,
            "project_uuid": uuid,
            "revision": 1,
            "settings": {"label": "Legacy label"},
        }))
        .unwrap(),
    )
    .expect("write legacy");
    let original = std::fs::read(&path).expect("read back");

    let result = read_settings(&root).expect("read_settings");
    assert!(result["operation_id"].is_null());
    assert_eq!(std::fs::read(&path).expect("re-read"), original);
}

#[test]
fn legacy_policy_operation_identity_defaults_without_rewriting() {
    let dir = testkit::scratch("settings", "legacy-policy");
    let root = project(&dir);
    let uuid = project_uuid(&root);
    let path = root.join("crawl-policy.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "format": POLICY_FORMAT,
            "project_uuid": uuid,
            "revision": 1,
            "policy": default_policy(),
        }))
        .unwrap(),
    )
    .expect("write legacy");
    let original = std::fs::read(&path).expect("read back");

    let result = project_policy(&root, None, false, None, None).expect("policy");
    assert!(result["operation_id"].is_null());
    assert_eq!(std::fs::read(&path).expect("re-read"), original);
}

#[test]
fn legacy_proposal_metadata_defaults_without_rewriting() {
    let dir = testkit::scratch("settings", "legacy-proposals");
    let root = project(&dir);
    let uuid = project_uuid(&root);
    let created_at = "2026-10-11T10:00:00Z";
    let path = root.join(PROPOSALS_FILE);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "format": PROPOSALS_FORMAT,
            "version": 1,
            "project_uuid": uuid,
            "revision": 1,
            "proposals": [
                {
                    "id": "a".repeat(32),
                    "created_at": created_at,
                    "client": "mcp",
                    "status": "pending",
                    "kind": "project_settings",
                    "base_revision": 0,
                    "changes": {"label": "Later"},
                    "decided_at": null,
                },
                {
                    "id": "b".repeat(32),
                    "created_at": created_at,
                    "client": "mcp",
                    "status": "applied",
                    "kind": "project_settings",
                    "base_revision": 0,
                    "changes": {"label": "Earlier"},
                    "decided_at": created_at,
                },
            ],
        }))
        .unwrap(),
    )
    .expect("write legacy");
    let original = std::fs::read(&path).expect("read back");

    let result = list_proposals(&root, 0, 50).expect("list");
    let items = result["items"].as_array().expect("items");
    let by_id = |id: &str| {
        items
            .iter()
            .find(|item| item["id"].as_str() == Some(id))
            .unwrap()
            .clone()
    };
    let a = by_id(&"a".repeat(32));
    let b = by_id(&"b".repeat(32));
    assert!(a["action_sequence"].is_null());
    assert!(a["decision"].is_null());
    assert!(b["action_sequence"].is_null());
    assert_eq!(b["decision"], "approve");
    assert_eq!(std::fs::read(&path).expect("re-read"), original);
}

#[test]
fn mcp_settings_proposal_needs_local_owner_review() {
    let dir = testkit::scratch("settings", "proposal-flow");
    let root = project(&dir);
    let created =
        propose_settings(&root, &json!({"label": "Proposed label"}), "mcp", 0).expect("propose");
    assert_eq!(created["proposal"]["status"], "pending");
    assert_eq!(
        read_settings(&root).expect("read")["settings"]["label"],
        "Example"
    );

    let reviewed = review_proposal(
        &root,
        created["proposal"]["id"].as_str().unwrap(),
        "approve",
        created["revision"].as_i64().unwrap(),
        "mcp",
    )
    .expect("review");
    assert_eq!(reviewed["proposal"]["status"], "applied");
    let settings = read_settings(&root).expect("read");
    assert_eq!(settings["settings"]["label"], "Proposed label");
    assert_eq!(settings["operation_id"], created["proposal"]["id"]);
    assert_eq!(
        list_proposals(&root, 0, 50).expect("list")["items"][0]["id"],
        created["proposal"]["id"]
    );
    assert_eq!(
        action_page(&root, 0, 50, Some("mcp"), Some("applied"), "").expect("actions")["total"],
        1
    );
}

#[test]
fn settings_proposal_lock_fails_fast_when_contended() {
    let dir = testkit::scratch("settings", "lock-contended");
    let root = project(&dir);
    let held = hold_write_lock(&root);
    held.recv().expect("holder acquired");
    let err =
        propose_settings(&root, &json!({"label": "Pending"}), "mcp", 0).expect_err("contended");
    assert!(err.to_string().contains("busy"), "got: {err}");
}

#[test]
fn failed_proposal_write_resolves_its_reserved_action_row() {
    let dir = testkit::scratch("settings", "failed-write");
    let root = project(&dir);
    // The document sentinel `.{name}.lock` makes only the proposals write
    // fail, standing in for the monkeypatched `write_document` OSError.
    let sentinel = root.join(format!(".{PROPOSALS_FILE}.lock"));
    std::fs::write(&sentinel, "held").expect("sentinel");
    let err = propose_settings(&root, &json!({"label": "Not persisted"}), "mcp", 0)
        .expect_err("proposal store failure");
    assert!(err.to_string().contains("another writer"), "got: {err}");
    std::fs::remove_file(&sentinel).expect("restore");

    assert_eq!(list_proposals(&root, 0, 50).expect("list")["total"], 0);
    assert_eq!(
        action_page(&root, 0, 50, Some("mcp"), Some("refused"), "").expect("actions")["total"],
        1
    );
}

/// Seed the crashed-finalize mid-state: the reserved proposal row persisted
/// as `applying` and the domain write already carried `operation_id`.
fn seed_applying(root: &std::path::Path, proposal: &Value) -> i64 {
    let path = root.join(PROPOSALS_FILE);
    let mut doc: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("proposals doc"))
            .expect("proposals json");
    let mut p = proposal.clone();
    p["status"] = json!("applying");
    p["decision"] = json!("approve");
    doc["proposals"] = json!([p]);
    doc["revision"] = json!(doc["revision"].as_i64().unwrap_or(0) + 1);
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).expect("seed applying");
    doc["revision"].as_i64().unwrap()
}

#[test]
fn owner_review_recovers_after_setting_applied_but_proposal_finalize_failed() {
    let dir = testkit::scratch("settings", "recover-label");
    let root = project(&dir);
    let created =
        propose_settings(&root, &json!({"label": "Recovery"}), "mcp", 0).expect("propose");
    let pid = created["proposal"]["id"].as_str().unwrap().to_string();
    // The settings write lands with the proposal id; the finalize write is
    // lost (the injected OSError) leaving status `applying`.
    update_label(&root, "Recovery", 0, "mcp", Some(&pid), false).expect("apply label");
    let revision = seed_applying(&root, &created["proposal"]);
    assert_eq!(
        read_settings(&root).expect("read")["settings"]["label"],
        "Recovery"
    );
    let applying = list_proposals(&root, 0, 50).expect("list");
    assert_eq!(applying["items"][0]["status"], "applying");

    let recovered =
        review_proposal(&root, &pid, "approve", revision, "mcp").expect("recovered review");
    assert_eq!(recovered["proposal"]["status"], "applied");
    assert_eq!(
        action_page(&root, 0, 50, Some("mcp"), Some("applied"), "").expect("actions")["total"],
        1
    );
}

#[test]
fn policy_proposal_retry_uses_persisted_operation_id() {
    let dir = testkit::scratch("settings", "recover-policy");
    let root = project(&dir);
    let mut policy = default_policy();
    policy["approval_thresholds"]["pages"] = json!(777);
    let created = propose_settings(&root, &json!({"crawl_policy": policy.clone()}), "mcp", 0)
        .expect("propose");
    let pid = created["proposal"]["id"].as_str().unwrap().to_string();
    project_policy(&root, Some(&policy), true, Some(0), Some(&pid)).expect("apply policy");
    let revision = seed_applying(&root, &created["proposal"]);
    let current = project_policy(&root, None, false, None, None).expect("policy");
    assert_eq!(current["operation_id"], pid);

    let applying = list_proposals(&root, 0, 50).expect("list");
    let recovered = review_proposal(
        &root,
        &pid,
        "approve",
        applying["revision"].as_i64().unwrap().max(revision),
        "mcp",
    )
    .expect("recovered review");
    assert_eq!(recovered["proposal"]["status"], "applied");
    assert_eq!(
        action_page(&root, 0, 50, Some("mcp"), Some("applied"), "").expect("actions")["total"],
        1
    );
}

#[test]
fn project_delete_moves_exports_to_sibling_trash() {
    let dir = testkit::scratch("settings", "delete-trash");
    let root = project(&dir);
    std::fs::create_dir_all(root.join("reports")).expect("reports");
    std::fs::write(root.join("reports").join("kept.txt"), "synthetic").expect("export");

    let result = delete_project(&root, "cli").expect("delete");
    let trash_path = std::path::PathBuf::from(result["trash_path"].as_str().unwrap());
    assert_eq!(result["exports_retained"], true);
    assert!(!root.exists());
    assert_eq!(
        std::fs::read_to_string(trash_path.join("reports").join("kept.txt")).expect("trash file"),
        "synthetic"
    );
    assert_eq!(
        action_page(&trash_path, 0, 50, Some("cli"), Some("deleted"), "").expect("actions")
            ["total"],
        1
    );
}

#[test]
fn project_delete_refuses_a_live_run() {
    let dir = testkit::scratch("settings", "delete-live");
    let root = project(&dir);
    std::fs::create_dir_all(root.join("scans")).expect("scans");
    run_observation::start(
        &root,
        "native",
        "spider",
        10,
        20_000,
        3_600,
        None,
        "synthetic",
        Some(root.join("scans").join("live.sqlite").to_str().unwrap()),
        false,
        None,
        None,
        None,
        None,
    )
    .expect("start run");

    let err = delete_project(&root, "cli").expect_err("active run");
    assert!(err.to_string().contains("active run"), "got: {err}");
    assert!(root.is_dir());
}

#[test]
fn project_delete_does_not_move_while_shared_writer_lock_is_unavailable() {
    let dir = testkit::scratch("settings", "delete-busy");
    let root = project(&dir);
    let held = hold_write_lock(&root);
    held.recv().expect("holder acquired");
    let err = delete_project(&root, "cli").expect_err("busy");
    assert!(err.to_string().contains("another writer"), "got: {err}");
    assert!(root.is_dir());
}

#[test]
fn project_delete_refuses_each_active_domain_job() {
    for (kind, lock_path) in [
        ("competitor", "competitor"),
        ("export", "export"),
        ("schedule", "schedule"),
    ] {
        let dir = testkit::scratch("settings", &format!("delete-{kind}"));
        let root = project(&dir);
        let lock_file = match lock_path {
            "competitor" => {
                locks::open_lock(&root.join(".competitor-scan.lock"), true).expect("lock file")
            }
            "export" => {
                let jobs = root.join("reports").join(".export-jobs");
                std::fs::create_dir_all(&jobs).expect("jobs dir");
                locks::open_lock(
                    &jobs.join("00000000-0000-4000-8000-000000000099.lock"),
                    true,
                )
                .expect("lock file")
            }
            _ => locks::open_lock(&root.join(".schedule.lock"), true).expect("lock file"),
        };
        locks::lock_exclusive(&lock_file).expect("flock");
        let err = delete_project(&root, "cli").expect_err("active jobs");
        assert!(err.to_string().contains("active jobs"), "got: {err}");
        assert!(root.is_dir());
        locks::unlock(&lock_file).expect("unlock");
    }
}

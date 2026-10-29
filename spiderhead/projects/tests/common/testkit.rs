//! Shared scratch/live-project helpers for the ported reference behaviour tests.
//!
//! Included per test binary via `#[path = "common/testkit.rs"] mod testkit;` —
//! separate from the fixture-replay `common/mod.rs` so binaries that need only
//! one side don't compile the other.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Per-test scratch directory, removed on drop.
pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Fresh scratch dir under the system temp dir, named after the test.
pub fn scratch(tag: &str, name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("sh-proj-{tag}-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    Scratch(dir)
}

/// The reference `project` fixture: `create_project` inside a scratch dir.
pub fn project(tag: &str, name: &str) -> (Scratch, PathBuf) {
    let dir = scratch(tag, name);
    let root = dir.0.join("site");
    spiderhead_projects::workspace::create_project(
        &root,
        "https://example.test/",
        None,
        None,
        None,
        None,
    )
    .expect("create_project");
    (dir, root)
}

/// Path to a bundled fixture file.
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Project + retained *running* native scan + real registered `native/spider`
/// run — the persistence-layer counterpart of the `_live_project` monkeypatch
/// (`open_scan` + `run_observation.status` fakes) used by the reference tests.
/// Returns `(scratch, project_root, run_id)`.
pub fn live_project(tag: &str, name: &str) -> (Scratch, PathBuf, String) {
    let (dir, root) = project(tag, name);
    std::fs::create_dir_all(root.join("scans")).expect("scans dir");
    let scan = root.join("scans").join("scan.sqlite");
    std::fs::copy(fixture("running-scan.sqlite"), &scan).expect("running scan fixture");
    let run = spiderhead_projects::run_observation::start(
        &root,
        "native",
        "spider",
        200,
        20_000,
        0,
        None,
        "fingerprint",
        Some(scan.to_str().expect("scan path")),
        false,
        None,
        None,
        None,
        None,
    )
    .expect("run_observation::start");
    let run_id = run["id"].as_str().expect("run id").to_owned();
    (dir, root, run_id)
}

/// Rewrite `run-observation.json` in place — the persistence-layer counterpart
/// of the reference tests monkeypatching `run_observation.status` rows.
pub fn doctor_runs(root: &Path, mutate: impl FnOnce(&mut Value)) {
    let path = root.join("run-observation.json");
    let mut doc: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("runs document"))
            .expect("runs json");
    mutate(&mut doc);
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).expect("write runs");
}

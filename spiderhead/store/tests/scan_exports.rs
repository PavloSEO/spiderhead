//! Tests of the reference suite: legacy exports from scan.v1 stay deterministic
//! and safely published. The fixture under `testdata/legacy-run` is one offline
//! crawl (`audit.json`, `pages.jsonl`, `links.jsonl`) and the scan.v1 artifact
//! that importing it produced; the importer is gone, so the committed artifact
//! stands in for every run imported before its removal. The one reference test
//! that monkeypatches the JSONL writer to inject a failure is deferred: it needs
//! reference-only mechanics.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_store::exports::export_run;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/legacy-run")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-tests40-exports-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A writable copy of the committed artifact of the imported legacy run.
fn artifact(dir: &Path) -> PathBuf {
    let path = dir.join("scan.sqlite");
    fs::copy(fixture().join("scan.sqlite"), &path).expect("copy fixture");
    path
}

/// reference `_pages`: JSONL pages with the live-only `attempts` projection dropped.
fn pages(path: &Path) -> Vec<Value> {
    fs::read_to_string(path.join("pages.jsonl"))
        .expect("pages.jsonl")
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).expect("json row");
            if let Some(object) = row.as_object_mut() {
                object.remove("attempts");
            }
            row
        })
        .collect()
}

fn links(path: &Path) -> Vec<Value> {
    fs::read_to_string(path.join("links.jsonl"))
        .expect("links.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json row"))
        .collect()
}

#[test]
fn export_matches_the_captured_observations_and_audit_bytes() {
    let dir = scratch("roundtrip");
    let scan = artifact(&dir);
    let before = fs::read(&scan).expect("bytes");
    let exported = dir.join("exported");

    let result = export_run(&scan, &exported).expect("export");
    assert_eq!(
        Value::Object(result),
        json!({"ok": true, "path": exported.to_string_lossy(), "counts": {"pages": 2, "links": 3}})
    );
    assert_eq!(fs::read(&scan).expect("bytes"), before);
    let mut names: Vec<String> = fs::read_dir(&exported)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["audit.json", "links.jsonl", "pages.jsonl"]);
    assert_eq!(
        fs::read(exported.join("audit.json")).expect("audit"),
        fs::read(fixture().join("audit.json")).expect("audit")
    );
    assert_eq!(pages(&exported), pages(&fixture()));
    assert_eq!(links(&exported), links(&fixture()));
}

#[test]
fn repeated_exports_are_byte_identical() {
    let dir = scratch("repeat");
    let scan = artifact(&dir);
    let first = dir.join("first");
    let second = dir.join("second");
    export_run(&scan, &first).expect("first");
    export_run(&scan, &second).expect("second");
    for name in ["pages.jsonl", "links.jsonl", "audit.json"] {
        assert_eq!(
            fs::read(first.join(name)).expect("first"),
            fs::read(second.join(name)).expect("second"),
            "{name}"
        );
    }
}

// Symlink creation needs privilege on Windows.
#[test]
#[cfg(unix)]
fn existing_and_symlinked_destinations_are_refused() {
    let dir = scratch("refuse");
    let scan = artifact(&dir);
    let existing = dir.join("existing");
    fs::create_dir_all(&existing).expect("existing");
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&existing, &alias).expect("symlink");
    for target in [&existing, &alias] {
        let err = export_run(&scan, target).expect_err("must refuse");
        assert!(
            err.to_string().contains("destination already exists"),
            "{err}"
        );
    }
    assert!(existing.is_dir());
    assert!(alias
        .symlink_metadata()
        .expect("meta")
        .file_type()
        .is_symlink());
}

#[test]
fn recovery_only_partialness_cannot_be_exported_as_complete() {
    let dir = scratch("recovery");
    let scan = dir.join("recovered.sqlite");
    fs::copy(fixture().join("scan-partial.sqlite"), &scan).expect("copy fixture");
    let before = fs::read(&scan).expect("bytes");
    let destination = dir.join("cannot-represent-recovery");
    let err = export_run(&scan, &destination).expect_err("must refuse");
    assert!(
        err.to_string()
            .contains("would hide recovered crawl partialness"),
        "{err}"
    );
    assert!(!destination.exists());
    assert_eq!(fs::read(&scan).expect("bytes"), before);
}

//!
//! Runs against a copy of the oracle fixture `oracle/raw-scans/basic/scan.sqlite`, never the
//! fixture itself. The "missing bodies" case is deferred: it needs a scan written with
//! `body_mode=off`, and the Rust scan writer (`NativeScan::create`) is not exposed yet.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_core::filesystem::file_sha256;
use spiderhead_store::scan_artifact::open_scan;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-store-ported-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A private copy of the basic oracle scan, so tests can rewrite its capability row.
fn basic_copy(dir: &Path) -> PathBuf {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/basic/scan.sqlite");
    let target = dir.join("scan.sqlite");
    fs::copy(&source, &target).unwrap();
    target
}

/// Read `offline_reanalysis` the way the reference helper does: open without audit, then read the
/// scan row.
fn capability(path: &Path) -> Value {
    let con = open_scan(path, false, None).unwrap();
    let raw: String = con
        .query_row("SELECT capabilities_json FROM scan", [], |row| row.get(0))
        .unwrap();
    let capabilities: Value = serde_json::from_str(&raw).unwrap();
    capabilities["offline_reanalysis"].clone()
}

/// Rewrite the stored capability, as an older writer would have recorded it.
fn set_capability(path: &Path, state: &str) {
    let con = Connection::open(path).unwrap();
    let raw: String = con
        .query_row("SELECT capabilities_json FROM scan", [], |row| row.get(0))
        .unwrap();
    let mut capabilities: Value = serde_json::from_str(&raw).unwrap();
    let reason = if state == "unavailable" {
        "older writer"
    } else {
        ""
    };
    capabilities["offline_reanalysis"] = json!({ "state": state, "reason": reason });
    con.execute(
        "UPDATE scan SET capabilities_json=?1",
        [capabilities.to_string()],
    )
    .unwrap();
}

#[test]
fn native_capability_supports_retained_raw_and_serialized_dom() {
    let dir = scratch("native-capability");
    let path = basic_copy(&dir);
    assert_eq!(capability(&path)["state"], "complete");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn old_unavailable_disclosure_is_read_without_upgrade() {
    let dir = scratch("old-disclosure");
    let path = basic_copy(&dir);
    set_capability(&path, "unavailable");
    let before = file_sha256(&path).unwrap();
    assert_eq!(
        capability(&path),
        json!({ "state": "unavailable", "reason": "older writer" })
    );
    assert_eq!(file_sha256(&path).unwrap(), before);
    fs::remove_dir_all(&dir).unwrap();
}

//! cases — a blocked scan-manifest writer fails clear and keeps the old file,
//! and manifest writes are atomic for concurrent readers. The three
//! `mcp_control` state-file cases belong to the mcp slice.
//!
//! reference monkeypatches `project_write_lock` to shorten the losing writer's
//! patience; the port instead holds the winning lock longer than the fixed
//! 5 s timeout — same contention, no seam needed.

#[path = "common/testkit.rs"]
mod testkit;

use std::sync::mpsc::channel;
use std::time::Duration;

use serde_json::{json, Value};
use spiderhead_projects::competitor_scans::{write_manifest, RUN_DIRECTORY};
use spiderhead_projects::runtime::project_write_lock;

#[test]
fn blocked_scan_manifest_writer_fails_clear_and_keeps_the_old_file() {
    let dir = testkit::scratch("concurrency", "blocked-writer");
    let root = dir.0.clone();
    std::fs::create_dir_all(root.join("reports")).expect("reports dir");

    let first = json!({"run_id": uuid::Uuid::new_v4().to_string(), "state": "running"});
    write_manifest(&root, &first).expect("first manifest");
    let manifest_path = std::fs::read_dir(root.join(RUN_DIRECTORY))
        .expect("run dir")
        .next()
        .expect("one manifest")
        .expect("entry")
        .path();

    // Holder thread: a real flock holder on a second descriptor.
    let (locked_tx, locked_rx) = channel::<()>();
    let (release_tx, release_rx) = channel::<()>();
    let hold_root = root.clone();
    let holder = std::thread::spawn(move || {
        let _guard = project_write_lock(&hold_root, 30.0).expect("holder lock");
        locked_tx.send(()).expect("signal");
        let _ = release_rx.recv_timeout(Duration::from_secs(30));
    });
    locked_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("locked");

    let second = json!({"run_id": uuid::Uuid::new_v4().to_string(), "state": "running"});
    let second_path = root
        .join(RUN_DIRECTORY)
        .join(format!("{}.json", second["run_id"].as_str().unwrap()));
    // The loser's 5 s deadline is below the holder's hold-until-release wait.
    let loser = {
        let root = root.clone();
        let second = second.clone();
        std::thread::spawn(move || write_manifest(&root, &second))
    };
    // Release the holder just after the loser's deadline elapses.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs_f64(6.0));
        release_tx.send(()).expect("release");
    });
    let err = loser.join().expect("loser join").expect_err("must refuse");
    assert!(
        err.to_string().contains("another writer owns it"),
        "got: {err}"
    );
    assert!(
        !second_path.exists(),
        "the refused write left nothing behind"
    );
    holder.join().expect("holder join");

    let kept: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("manifest"))
            .expect("manifest json");
    assert_eq!(kept, first, "the losing writer left no partial content");
    write_manifest(&root, &second).expect("write after release");
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&second_path).expect("second manifest"))
            .expect("second json");
    assert_eq!(written, second);
    let still: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("manifest"))
            .expect("manifest json");
    assert_eq!(still, first);
}

#[test]
fn scan_manifest_write_is_atomic_for_concurrent_readers() {
    let dir = testkit::scratch("concurrency", "atomic-readers");
    let root = dir.0.clone();
    std::fs::create_dir_all(root.join("reports")).expect("reports dir");
    let manifest = json!({"run_id": uuid::Uuid::new_v4().to_string(), "state": "running"});
    write_manifest(&root, &manifest).expect("manifest");
    let manifest_path = std::fs::read_dir(root.join(RUN_DIRECTORY))
        .expect("run dir")
        .next()
        .expect("one manifest")
        .expect("entry")
        .path();

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let torn = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let stop = std::sync::Arc::clone(&stop);
        let torn = std::sync::Arc::clone(&torn);
        let manifest_path = manifest_path.clone();
        let run_id = manifest["run_id"].as_str().unwrap().to_owned();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                match std::fs::read_to_string(&manifest_path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                {
                    Some(document) if document["run_id"].as_str() == Some(&run_id) => {}
                    _ => {
                        torn.store(true, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                }
            }
        })
    };
    for _ in 0..30 {
        write_manifest(&root, &manifest).expect("write_manifest");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    reader.join().expect("reader join");
    assert!(!torn.load(std::sync::atomic::Ordering::Relaxed));
}

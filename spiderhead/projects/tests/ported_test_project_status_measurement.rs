//! observations are persisted and read without a directory walk.

#[path = "common/testkit.rs"]
mod testkit;

use spiderhead_projects::status_measurement::{
    read_storage_measurement, refresh_storage_measurement,
};

#[test]
fn storage_measurement_is_cached_and_reports_direct_scan_files() {
    let (_dir, root) = testkit::project("measure", "cached");
    std::fs::create_dir_all(root.join("scans")).expect("scans dir");
    std::fs::write(root.join("scans/first.sqlite"), b"synthetic scan").expect("scan");
    std::fs::write(root.join("scans/first.sqlite-wal"), b"wal").expect("wal");

    let measured = refresh_storage_measurement(&root).expect("refresh");
    let cached = read_storage_measurement(&root).expect("read");

    assert_eq!(measured["state"], "available");
    assert_eq!(
        measured["total_bytes"].as_i64().unwrap(),
        (b"synthetic scan".len() + b"wal".len()) as i64
    );
    assert_eq!(measured["scan_count"].as_i64().unwrap(), 1);
    assert_eq!(cached, measured);
}

#[test]
fn nested_scan_directory_is_explicitly_unavailable() {
    let (_dir, root) = testkit::project("measure", "nested");
    std::fs::create_dir_all(root.join("scans/nested")).expect("nested dir");

    let measured = refresh_storage_measurement(&root).expect("refresh");

    assert_eq!(measured["state"], "unavailable");
    assert!(measured["total_bytes"].is_null());
    assert!(measured["reason"].as_str().unwrap().contains("nested"));
}

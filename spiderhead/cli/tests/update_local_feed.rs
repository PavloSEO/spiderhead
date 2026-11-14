//! `spiderhead update` against a local release feed: install, no-op, rollback, corrupted asset.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const SCRIPT: &[u8] = b"#!/bin/sh\necho spiderhead 0.10.1\n";

fn triple() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        _ => "aarch64-unknown-linux-gnu",
    }
}

/// A feed with one release `v0.10.1` whose binary is `asset`, listed in SHA256SUMS as `listed`.
fn feed(root: &Path, asset: &[u8], listed: &[u8]) {
    let dir = root.join("download/v0.10.1");
    std::fs::create_dir_all(&dir).expect("feed dir");
    let name = format!("spiderhead-0.10.1-{}", triple());
    std::fs::write(dir.join(&name), asset).expect("asset");
    let digest = format!("{:x}", Sha256::digest(listed));
    std::fs::write(dir.join("SHA256SUMS"), format!("{digest}  {name}\n")).expect("sums");
}

fn update(root: &Path, bin: &Path, extra: &[&str]) -> (i32, serde_json::Value, Output) {
    update_as(root, bin, extra, "0.10.0")
}

fn update_as(
    root: &Path,
    bin: &Path,
    extra: &[&str],
    current: &str,
) -> (i32, serde_json::Value, Output) {
    let output = Command::new(env!("CARGO_BIN_EXE_spiderhead"))
        .args(["update", "--feed"])
        .arg(root.join("feed"))
        .arg("--bin-dir")
        .arg(bin)
        .args(extra)
        .env("SPIDERHEAD_UPDATE_PRETEND_VERSION", current)
        .env(
            "SEOHEAD_UPDATE_STATE_PATH",
            root.join("state/update-state.json"),
        )
        .output()
        .expect("run spiderhead");
    let json = serde_json::from_slice(&output.stdout).unwrap_or(serde_json::Value::Null);
    (output.status.code().unwrap_or(-1), json, output)
}

fn version_of(binary: &Path) -> String {
    let out = Command::new(binary).arg("--version").output().expect("run");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn local_feed_installs_then_rolls_back() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let target = bin.join("spiderhead");
    std::fs::write(&target, b"#!/bin/sh\necho spiderhead 0.10.0\n").expect("old");
    std::fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    feed(&root.join("feed"), SCRIPT, SCRIPT);

    let (code, plan, _) = update(root, &bin, &["--dry-run"]);
    assert_eq!((code, plan["state"].as_str()), (0, Some("available")));
    assert_eq!(version_of(&target), "spiderhead 0.10.0");

    let (code, done, _) = update(root, &bin, &[]);
    assert_eq!(
        (code, done["state"].as_str()),
        (0, Some("installed")),
        "{done}"
    );
    assert_eq!(done["signature"], "unsigned");
    assert_eq!(version_of(&target), "spiderhead 0.10.1");
    assert!(bin.join(".spiderhead-update/spiderhead.prev").exists());

    // Once the running version is 0.10.1 there is nothing newer.
    let (code, again, _) = update_as(root, &bin, &[], "0.10.1");
    assert_eq!((code, again["state"].as_str()), (0, Some("current")));

    let (code, back, _) = update(root, &bin, &["--rollback"]);
    assert_eq!((code, back["state"].as_str()), (0, Some("restored")));
    assert_eq!(version_of(&target), "spiderhead 0.10.0");
}

#[test]
fn corrupted_asset_is_refused_and_the_binary_is_untouched() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let target = bin.join("spiderhead");
    std::fs::write(&target, b"old").expect("old");
    // The listed digest is of the genuine script; the asset in the feed differs by one byte.
    feed(
        &root.join("feed"),
        b"#!/bin/sh\necho spiderhead 0.10.2\n",
        SCRIPT,
    );

    let (code, answer, _) = update(root, &bin, &[]);
    assert_eq!(
        (code, answer["state"].as_str()),
        (1, Some("checksum_mismatch")),
        "{answer}"
    );
    assert_eq!(std::fs::read(&target).expect("read"), b"old");
    assert!(!bin.join(".spiderhead-update/spiderhead.new").exists());
}

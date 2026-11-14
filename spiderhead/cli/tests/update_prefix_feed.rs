//! `spiderhead update` of an `install.sh`-style prefix (`<prefix>/lib/spiderhead/bin`) from a local
//! feed holding the profile `.tar.gz`: install, rollback, and a hostile archive.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// The release under test carries this build itself, so its `--version` matches the release and
/// the updated prefix can still run `--rollback`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn cli() -> Vec<u8> {
    std::fs::read(env!("CARGO_BIN_EXE_spiderhead")).expect("binary")
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn run(cmd: &mut Command) {
    assert!(cmd.status().expect("run").success());
}

/// A prefix whose installed CLI is a copy of this build, with a `cli` profile.
fn prefix(root: &Path) -> PathBuf {
    let bin = root.join("p/lib/spiderhead/bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::create_dir_all(root.join("p/share/spiderhead")).expect("share");
    std::fs::write(
        root.join("p/share/spiderhead/profile.toml"),
        "profile = \"cli\"\n",
    )
    .expect("profile");
    std::fs::write(bin.join("old-marker"), b"old").expect("marker");
    std::fs::copy(env!("CARGO_BIN_EXE_spiderhead"), bin.join("spiderhead")).expect("copy");
    bin.join("spiderhead")
}

/// A feed with the current release holding the profile archive built from `files` (path, bytes);
/// `extra` are further tar arguments (taken verbatim, `-P`).
fn feed(root: &Path, files: &[(&str, &[u8])], extra: &[&str]) {
    let src = root.join("src");
    let mut sums = String::new();
    for (path, bytes) in files {
        let file = src.join("cli").join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
        std::fs::write(&file, bytes).expect("file");
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        sums.push_str(&format!("{}  {path}\n", sha(bytes)));
    }
    std::fs::write(src.join("cli/SHA256SUMS"), sums).expect("sums");
    std::fs::write(src.join("evil"), b"evil").expect("evil");
    let dir = root.join(format!("feed/download/v{VERSION}"));
    std::fs::create_dir_all(&dir).expect("feed");
    let name = format!(
        "spiderhead-{VERSION}-cli-{}-{}.tar.gz",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    run(Command::new("tar")
        .args(["-czPf"])
        .arg(dir.join(&name))
        .arg("-C")
        .arg(&src)
        .arg("cli/bin")
        .arg("cli/SHA256SUMS")
        .args(extra));
    let bytes = std::fs::read(dir.join(&name)).expect("archive");
    std::fs::write(dir.join("SHA256SUMS"), format!("{}  {name}\n", sha(&bytes))).expect("sums");
}

fn update(root: &Path, exe: &Path, extra: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(exe)
        .args(["update", "--feed"])
        .arg(root.join("feed"))
        .args(extra)
        .env("SPIDERHEAD_UPDATE_PRETEND_VERSION", "0.0.1")
        .env(
            "SEOHEAD_UPDATE_STATE_PATH",
            root.join("state/update-state.json"),
        )
        .output()
        .expect("run");
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), json)
}

#[test]
fn prefix_install_replaces_the_bin_directory_and_rolls_back() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    let exe = prefix(root);
    let bin = exe.parent().expect("bin").to_path_buf();
    feed(
        root,
        &[("bin/spiderhead", &cli()), ("bin/new-marker", b"new")],
        &[],
    );

    let (code, done) = update(root, &exe, &[]);
    assert_eq!(
        (code, done["state"].as_str()),
        (0, Some("installed")),
        "{done}"
    );
    assert_eq!(done["kind"], "prefix");
    assert!(bin.join("new-marker").exists() && !bin.join("old-marker").exists());
    let version = Command::new(bin.join("spiderhead"))
        .arg("--version")
        .output()
        .expect("run");
    assert!(String::from_utf8_lossy(&version.stdout).contains(VERSION));

    // The restored directory holds the old files again (the old CLI is the real build).
    let (code, back) = update(root, &bin.join("spiderhead"), &["--rollback"]);
    assert_eq!(
        (code, back["state"].as_str()),
        (0, Some("restored")),
        "{back}"
    );
    assert!(bin.join("old-marker").exists() && !bin.join("new-marker").exists());
}

#[test]
fn hostile_archive_is_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    let exe = prefix(root);
    let bin = exe.parent().expect("bin").to_path_buf();
    feed(root, &[("bin/spiderhead", &cli())], &["cli/../evil"]);

    let (code, answer) = update(root, &exe, &[]);
    assert_eq!(
        (code, answer["state"].as_str()),
        (1, Some("install_failed")),
        "{answer}"
    );
    assert!(
        answer["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("unsafe archive"),
        "{answer}"
    );
    assert!(bin.join("old-marker").exists());
    assert!(!root.join("p/lib/spiderhead/evil").exists() && !root.join("p/lib/evil").exists());
}

//! one per-host schedule through the user state directory.
//!
//! `SEOHEAD_CONFIG_DIR` maps to the Rust `SPIDERHEAD_CONFIG_DIR`. The
//! `os.name == "nt"` emulation of the store-mode gate (#1345) is a
//! reference-only seam; the POSIX rejection half is ported.

#[path = "common/testkit.rs"]
mod testkit;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use spiderhead_projects::origin_pacing::ProjectOriginPacer;

const TARGET: &str = "https://example.test/start";

// Env mutation is process-global: serialise every test that sets the state dir.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn state(tag: &str, name: &str) -> (testkit::Scratch, PathBuf) {
    let dir = testkit::scratch(tag, name);
    let state = dir.0.join("state").join("spiderhead");
    std::env::set_var("SPIDERHEAD_CONFIG_DIR", &state);
    (dir, state)
}

#[test]
fn projectless_pacer_stores_schedule_in_user_state_dir() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let (_dir, state) = state("pacing", "state-dir");
    let pacer = ProjectOriginPacer::new(None, TARGET, 0.0, 4.0).expect("pacer");

    let path = state.join(".origin-pacing.sqlite");
    assert_eq!(pacer.reserve().expect("reserve"), 0.0);
    assert!(path.exists(), "schedule should live in the state dir");
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn projectless_pacers_share_one_host_schedule() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let (_dir, _state) = state("pacing", "shared");
    let first = ProjectOriginPacer::new(None, TARGET, 0.0, 2.0).expect("first");
    let second = ProjectOriginPacer::new(None, TARGET, 0.0, 2.0).expect("second");

    assert_eq!(first.reserve().expect("reserve"), 0.0);
    // A second process reserves the slot after the first one: about one interval later.
    let wait = second.reserve().expect("reserve");
    assert!((0.4..=0.5).contains(&wait), "wait was {wait}");
}

#[test]
fn projectless_unlimited_pacer_reserves_nothing() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let (_dir, state) = state("pacing", "unlimited");
    let pacer = ProjectOriginPacer::new(None, TARGET, 0.0, 0.0).expect("pacer");

    for _ in 0..5 {
        assert_eq!(pacer.reserve().expect("reserve"), 0.0);
    }
    assert!(!state.join(".origin-pacing.sqlite").exists());
}

// (posix row only; the "nt" row emulates os.name, a reference-only seam)
#[test]
#[cfg(unix)]
fn store_mode_gate_is_strict_where_modes_exist() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let (_dir, state) = state("pacing", "mode-gate");
    let pacer = ProjectOriginPacer::new(None, TARGET, 0.0, 1.0).expect("pacer");
    pacer.reserve().expect("reserve");
    let path = state.join(".origin-pacing.sqlite");
    // group/other-readable: a private-store violation on POSIX
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

    let err = pacer.reserve().expect_err("must reject");
    assert!(err.to_string().contains("must be private"), "got: {err}");
}

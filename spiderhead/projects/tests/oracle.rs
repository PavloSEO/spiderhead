//! Oracle parity: replay generated scenario fixtures through the Rust port.
mod common;

use common::run_fixture;

#[test]
fn workspace() {
    run_fixture("workspace.json");
}

#[test]
fn admin() {
    run_fixture("admin.json");
}

#[test]
fn scans() {
    run_fixture("scans.json");
}

#[test]
fn schedule() {
    run_fixture("schedule.json");
}

#[test]
fn views() {
    run_fixture("views.json");
}

#[test]
fn ops() {
    run_fixture("ops.json");
}

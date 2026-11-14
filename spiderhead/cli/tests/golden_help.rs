//! Every `--help` screen of the Rust CLI equals the reference one (stdout, stderr, exit code).

mod common;

#[test]
fn every_help_screen_matches_reference() {
    let n = common::assert_fixture("cli_help.json");
    assert!(n > 300, "expected the whole tree, got {n} screens");
}

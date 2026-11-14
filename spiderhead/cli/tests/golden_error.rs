//! Parse errors: same usage block, same message, same exit code as argparse.

mod common;

#[test]
fn every_parse_error_matches_reference() {
    let n = common::assert_fixture("cli_errors.json");
    assert!(n > 1000, "expected thousands of error cases, got {n}");
}

#[test]
fn top_level_invocations_match_python() {
    common::assert_fixture("cli_special.json");
}

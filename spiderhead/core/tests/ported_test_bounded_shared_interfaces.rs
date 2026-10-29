//! Ports of the pure-contract part of the reference suite. Each test names
//! its source. The CLI/MCP parity tests, the remote backend admission test (its HTTP and
//! SQLite half), the report, remediation and monitor tests need handler layers that are not
//! ported yet, so they are listed in the slice report instead.

use spiderhead_core::job_contracts::ScanOptions;

// (ScanOptions bounds half only; the backend half is deferred)
#[test]
fn scale_admission_options_reject_values_above_the_ceilings() {
    let base = ScanOptions::default();
    for over in [
        ScanOptions {
            max_urls: 1_000_001,
            ..base.clone()
        },
        ScanOptions {
            max_urls: 2_000_000,
            ..base.clone()
        },
        ScanOptions {
            max_requests: 2_000_001,
            ..base.clone()
        },
    ] {
        assert!(
            over.clone().validated().is_err(),
            "expected rejection for {over:?}"
        );
    }
}

// The authorized budget in that test (1,000,000 URLs, 2,000,000 requests) is accepted at the ceiling.
#[test]
fn scale_admission_authorized_budget_is_accepted_at_the_ceiling() {
    let authorized = ScanOptions {
        max_urls: 1_000_000,
        max_requests: 2_000_000,
        ..ScanOptions::default()
    };
    assert!(authorized.validated().is_ok());
}

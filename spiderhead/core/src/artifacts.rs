//! Format identifiers for scans, ledgers, projects and reports.
//!
//! These strings are part of the stored and exported interface: SQLite headers, JSON manifests and CLI/MCP output all carry them.
//! Older artifacts must keep validating, so the values never change. Writers and validators use these constants, not literals.

/// Scan format, version 1.
pub const SCAN_V1: &str = "scan.v1";
/// Scan format, version 2.
pub const SCAN_V2: &str = "scan.v2";
/// Every scan format this build can read. Membership: `SCAN_FORMATS.contains(&format)`.
pub const SCAN_FORMATS: [&str; 2] = [SCAN_V1, SCAN_V2];
/// Ledger artifact format, version 1.
pub const LEDGER_V1: &str = "ledger.v1";
/// Project manifest format, version 1.
pub const PROJECT_V1: &str = "spiderhead.project.v1";
/// Crawl timeline format, version 1.
pub const CRAWL_TIMELINE_V1: &str = "spiderhead.crawl-timeline.v1";
/// Comparison format, version 2.
pub const COMPARE_V2: &str = "compare.v2";
/// Audit format, version 2.
pub const AUDIT_V2: &str = "audit.v2";

#[cfg(test)]
mod tests {
    use super::*;

    /// Format identifiers are pinned: changing a value breaks older artifacts, so it is a format break, not a refactor.
    #[test]
    fn format_identifiers_are_pinned() {
        assert_eq!(SCAN_V1, "scan.v1");
        assert_eq!(SCAN_V2, "scan.v2");
        assert_eq!(LEDGER_V1, "ledger.v1");
        assert_eq!(PROJECT_V1, "spiderhead.project.v1");
        assert_eq!(CRAWL_TIMELINE_V1, "spiderhead.crawl-timeline.v1");
        assert_eq!(COMPARE_V2, "compare.v2");
        assert_eq!(AUDIT_V2, "audit.v2");
    }

    #[test]
    fn scan_formats_lists_both_scan_versions() {
        assert!(SCAN_FORMATS.contains(&SCAN_V1));
        assert!(SCAN_FORMATS.contains(&SCAN_V2));
        assert!(!SCAN_FORMATS.contains(&LEDGER_V1));
    }
}

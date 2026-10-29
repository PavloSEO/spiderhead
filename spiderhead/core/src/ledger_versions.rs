//! Ledger format constants: the format identifier and the highest supported SQLite user version.

use crate::artifacts::LEDGER_V1;

/// Format identifier written into the ledger header and checked when it is opened: `ledger.v1`.
pub const FORMAT_VERSION: &str = LEDGER_V1;

/// Highest SQLite `user_version` this build understands. Migrations advance a database up to it.
/// Signed to match the integer that `PRAGMA user_version` returns.
pub const USER_VERSION: i64 = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_format_and_user_version_match_the_reference_schema() {
        assert_eq!(FORMAT_VERSION, "ledger.v1");
        assert_eq!(USER_VERSION, 5);
    }
}

//! The artifact DDL, byte-identical to the checked-in SQL files.
//!
//! The SQL files are copied verbatim into `sql/`; validation compares the schema objects
//! (including embedded comments, which SQLite keeps in `sqlite_master.sql`) built from the same
//! text, so the bytes must not change. Hashes are pinned in the tests.

/// `scan.v1` DDL: pragmas (`application_id`, `user_version`, `foreign_keys`), tables, indexes.
pub const SCAN_V1_SQL: &str = include_str!("../sql/scan_v1.sql");
/// `scan.v2` write upgrade (retry history and later tables); never applied implicitly by readers.
pub const SCAN_V2_SQL: &str = include_str!("../sql/scan_v2.sql");
/// `audit.v2` companion database DDL (`scan.sqlite.audit-v2.sqlite`).
pub const AUDIT_V2_SQL: &str = include_str!("../sql/audit_v2.sql");
/// `ledger.v1` DDL (storage version 1).
pub const LEDGER_V1_SQL: &str = include_str!("../sql/ledger_v1.sql");
/// Ledger migration to storage version 2.
pub const LEDGER_V2_SQL: &str = include_str!("../sql/ledger_v2.sql");
/// Ledger migration to storage version 3.
pub const LEDGER_V3_SQL: &str = include_str!("../sql/ledger_v3.sql");
/// Ledger migration to storage version 4.
pub const LEDGER_V4_SQL: &str = include_str!("../sql/ledger_v4.sql");
/// Ledger migration to storage version 5 (statements are split on `;` by the migrator).
pub const LEDGER_V5_SQL: &str = include_str!("../sql/ledger_v5.sql");

/// scan.v1 `PRAGMA application_id` (ASCII `SEOH`).
pub const SCAN_APPLICATION_ID: i64 = 1_397_051_208;
/// scan.v1 `PRAGMA user_version`.
pub const SCAN_USER_VERSION: i64 = 1;
/// scan.v1 `scan.format_version` value.
pub const SCAN_FORMAT_VERSION: &str = "scan.v1";
/// audit.v2 `PRAGMA application_id` (ASCII `AUDV`).
pub const AUDIT_APPLICATION_ID: i64 = 1_096_107_094;
/// audit.v2 `PRAGMA user_version`.
pub const AUDIT_USER_VERSION: i64 = 3;
/// audit.v2 format name.
pub const AUDIT_FORMAT_VERSION: &str = "audit.v2";
/// ledger.v1 `PRAGMA application_id` (ASCII `SEOL`).
pub const LEDGER_APPLICATION_ID: i64 = 1_397_051_212;
/// Newest ledger `PRAGMA user_version` this build knows.
pub const LEDGER_USER_VERSION: i64 = 5;
/// Ledger format name.
pub const LEDGER_FORMAT_VERSION: &str = "ledger.v1";

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(s: &str) -> String {
        Sha256::digest(s.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn ddl_is_byte_identical_to_python_package_data() {
        // SHA-256 of the SQL files, pinned so any byte change is caught.
        let pins = [
            (
                SCAN_V1_SQL,
                "0d8f505e99b6abddb5a0b65eea6ed7926251e651f66d9ead8dd557a5060f04e7",
            ),
            (
                SCAN_V2_SQL,
                "689074662e081d746074b55ef2392025188f8f14e8fdc32a78e2c99de6834d89",
            ),
            (
                AUDIT_V2_SQL,
                "26765bab907db8b1da6668da61b5f0439b74e6b1b29af8d31742beb6d23282bf",
            ),
            (
                LEDGER_V1_SQL,
                "de541a20c3925e880ce09183c939e4bfe535453fc44b9741a84bc2a93b2d5197",
            ),
            (
                LEDGER_V2_SQL,
                "cbe25f0c6cfc0a39212ab1ad06806146a4c69f8c2007ee9c0c075ee0de752c55",
            ),
            (
                LEDGER_V3_SQL,
                "fc1eebca5e9088a332267bb74888df3da3f4d4f2cf5f7ccaab4f4e748fd5c91b",
            ),
            (
                LEDGER_V4_SQL,
                "4fa7ab36bbdc6e6fbab5fd9ecd6e1bb98b319a55447e6046facb1f7d9b205a7d",
            ),
            (
                LEDGER_V5_SQL,
                "3e25028d60f11db7a8c569b69d78db90518937794860bd893d17b6d7a9f0a23e",
            ),
        ];
        for (sql, want) in pins {
            assert_eq!(hex(sql), want);
        }
    }

    #[test]
    fn scan_v1_ddl_builds_and_sets_header_pragmas() {
        let c = rusqlite::Connection::open_in_memory().expect("open");
        c.execute_batch(SCAN_V1_SQL).expect("ddl");
        let tables: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 15);
        let uv: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("uv");
        assert_eq!(uv, SCAN_USER_VERSION);
    }

    #[test]
    fn ledger_chain_and_audit_build() {
        let c = rusqlite::Connection::open_in_memory().expect("open");
        c.execute_batch(LEDGER_V1_SQL).expect("v1");
        for sql in [LEDGER_V2_SQL, LEDGER_V3_SQL, LEDGER_V4_SQL, LEDGER_V5_SQL] {
            c.execute_batch(sql).expect("migration");
        }
        let a = rusqlite::Connection::open_in_memory().expect("open");
        a.execute_batch(AUDIT_V2_SQL).expect("audit");
    }
}

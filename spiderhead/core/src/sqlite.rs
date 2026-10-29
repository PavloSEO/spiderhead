//! SQLite opener helpers shared by the storage, data-source and check modules.
//!
//! Callers keep their own path normalisation and PRAGMAs; this module only removes the repeated
//! open, busy-timeout and pragma boilerplate. Rows are read through `rusqlite::Row`, so the opener
//! takes no row factory.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Busy timeout used when the caller does not choose one: 5 seconds.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Open an existing database read-only. The file is never created: a missing path is an error.
///
/// `path` must already be absolute; the caller resolves it.
pub fn open_readonly(
    path: &Path,
    timeout: Duration,
    pragmas: &[&str],
) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure(connection, timeout, pragmas)
}

/// Open a database for reading and writing, creating the file when it is missing.
pub fn open_writer(
    path: &Path,
    timeout: Duration,
    pragmas: &[&str],
) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    configure(connection, timeout, pragmas)
}

fn configure(
    connection: Connection,
    timeout: Duration,
    pragmas: &[&str],
) -> rusqlite::Result<Connection> {
    connection.busy_timeout(timeout)?;
    for statement in pragmas {
        connection.execute_batch(statement)?;
    }
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch directory unique to one test run; removed by the caller.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-core-sqlite-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn open_writer_creates_database_and_applies_pragmas() {
        let dir = scratch("writer");
        let path = dir.join("data.sqlite");
        {
            let con = open_writer(&path, DEFAULT_TIMEOUT, &["PRAGMA query_only=OFF"]).unwrap();
            con.execute_batch("CREATE TABLE item (value TEXT); INSERT INTO item VALUES ('a');")
                .unwrap();
            let value: String = con
                .query_row("SELECT value FROM item", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, "a");
        }
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_readonly_rejects_writes() {
        let dir = scratch("readonly");
        let path = dir.join("data.sqlite");
        {
            let seed = Connection::open(&path).unwrap();
            seed.execute_batch("CREATE TABLE item (value TEXT)")
                .unwrap();
        }
        let con = open_readonly(&path, DEFAULT_TIMEOUT, &["PRAGMA query_only=ON"]).unwrap();
        let count: i64 = con
            .query_row("SELECT count(*) FROM item", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert!(con.execute("INSERT INTO item VALUES ('x')", []).is_err());
        drop(con);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_readonly_does_not_create_missing_file() {
        let dir = scratch("missing");
        let path = dir.join("missing.sqlite");
        assert!(open_readonly(&path, DEFAULT_TIMEOUT, &[]).is_err());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

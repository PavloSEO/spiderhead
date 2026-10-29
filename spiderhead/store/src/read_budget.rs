//! Bound SQLite execution without charging caller work between fetched rows.
//!
//! `ReadConnection` is a thin wrapper over [`rusqlite::Connection`]. The per-operation budget is a
//! SQLite progress handler that interrupts the running statement once a deadline has passed; the
//! deadline is renewed before each statement and before each fetched row, so only time spent inside
//! SQLite counts, not time the caller spends between rows. SQL that must run unbudgeted goes
//! through [`ReadConnection::as_raw`], which documents that it skips the renewal.

use crate::error::StoreError;
use rusqlite::{Connection, Params, Row};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Progress-handler granularity: SQLite virtual-machine ops between deadline checks.
const PROGRESS_OPS: std::os::raw::c_int = 1000;

fn lock(deadline: &Mutex<Instant>) -> std::sync::MutexGuard<'_, Instant> {
    deadline.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start in validation mode; activate per-operation budgets after validation.
pub struct ReadConnection {
    con: Connection,
    query_timeout_seconds: Option<f64>,
    deadline: Arc<Mutex<Instant>>,
}

/// Cursor over a [`ReadConnection`]: every statement and every fetched row renews the budget.
pub struct ReadCursor<'a> {
    connection: &'a ReadConnection,
}

impl ReadConnection {
    /// Wrap an open connection; no budget is active until [`set_query_budget`](Self::set_query_budget).
    pub fn new(con: Connection) -> Self {
        Self {
            con,
            query_timeout_seconds: None,
            deadline: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// The raw connection for callers that must run outside the budget.
    ///
    /// Statements issued through this handle do not renew the deadline; once the last renewed
    /// deadline passes, the progress handler interrupts them mid-run.
    pub fn as_raw(&self) -> &Connection {
        &self.con
    }

    /// The configured per-operation budget in seconds, if any.
    pub fn query_timeout_seconds(&self) -> Option<f64> {
        self.query_timeout_seconds
    }

    /// Give every following statement and row `seconds` of SQLite time (finite and positive).
    pub fn set_query_budget(&mut self, seconds: f64) -> Result<(), StoreError> {
        if !seconds.is_finite() || seconds <= 0.0 || seconds > 1.0e9 {
            return Err(StoreError::Invalid(
                "SQLite query timeout must be a finite positive number".to_string(),
            ));
        }
        self.query_timeout_seconds = Some(seconds);
        self.renew_query_budget();
        let deadline = Arc::clone(&self.deadline);
        self.con.progress_handler(
            PROGRESS_OPS,
            Some(move || Instant::now() > *lock(&deadline)),
        );
        Ok(())
    }

    /// Restart the deadline from now (no-op while no budget is set).
    pub fn renew_query_budget(&self) {
        if let Some(seconds) = self.query_timeout_seconds {
            *lock(&self.deadline) = Instant::now() + Duration::from_secs_f64(seconds);
        }
    }

    /// Open a cursor whose statements and rows renew the budget.
    pub fn cursor(&self) -> ReadCursor<'_> {
        ReadCursor { connection: self }
    }

    /// Run one statement under a fresh budget; returns the changed-row count.
    pub fn execute<P: Params>(&self, sql: &str, parameters: P) -> Result<usize, StoreError> {
        self.cursor().execute(sql, parameters)
    }

    /// Run one statement per parameter set, each under a fresh budget.
    pub fn executemany<P: Params>(
        &self,
        sql: &str,
        parameters: impl IntoIterator<Item = P>,
    ) -> Result<usize, StoreError> {
        self.cursor().executemany(sql, parameters)
    }

    /// Run a query and map every row, renewing the budget before the statement and before each row.
    pub fn query_rows<T, P: Params>(
        &self,
        sql: &str,
        parameters: P,
        map: impl FnMut(&Row<'_>) -> Result<T, StoreError>,
    ) -> Result<Vec<T>, StoreError> {
        self.cursor().query_rows(sql, parameters, map)
    }
}

impl ReadCursor<'_> {
    /// Run one statement on this cursor under the budget.
    pub fn execute<P: Params>(&self, sql: &str, parameters: P) -> Result<usize, StoreError> {
        self.connection.renew_query_budget();
        Ok(self.connection.con.execute(sql, parameters)?)
    }

    /// Run one statement per parameter set on this cursor.
    pub fn executemany<P: Params>(
        &self,
        sql: &str,
        parameters: impl IntoIterator<Item = P>,
    ) -> Result<usize, StoreError> {
        self.connection.renew_query_budget();
        let mut statement = self.connection.con.prepare(sql)?;
        let mut changed = 0;
        for set in parameters {
            changed += statement.execute(set)?;
        }
        Ok(changed)
    }

    /// Collect every row of a freshly executed query, renewing the budget before each row.
    pub fn query_rows<T, P: Params>(
        &self,
        sql: &str,
        parameters: P,
        mut map: impl FnMut(&Row<'_>) -> Result<T, StoreError>,
    ) -> Result<Vec<T>, StoreError> {
        self.connection.renew_query_budget();
        let mut statement = self.connection.con.prepare(sql)?;
        let mut rows = statement.query(parameters)?;
        let mut out = Vec::new();
        loop {
            self.connection.renew_query_budget();
            match rows.next()? {
                Some(row) => out.push(map(row)?),
                None => return Ok(out),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counting() -> ReadConnection {
        ReadConnection::new(Connection::open_in_memory().expect("memory db"))
    }

    // Budget must be finite and positive.
    #[test]
    fn budget_must_be_finite_and_positive() {
        let mut con = counting();
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(con.set_query_budget(bad).is_err(), "{bad}");
        }
        assert!(con.query_timeout_seconds().is_none());
        con.set_query_budget(2.5).unwrap();
        assert_eq!(con.query_timeout_seconds(), Some(2.5));
    }

    // A runaway query is interrupted once the budget expires.
    #[test]
    fn runaway_query_is_interrupted_by_budget() {
        let mut con = counting();
        con.set_query_budget(0.05).unwrap();
        let result = con.query_rows(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT COUNT(*) FROM c",
            [],
            |row| Ok(row.get::<_, i64>(0)?),
        );
        assert!(matches!(result, Err(StoreError::Sqlite(_))), "{result:?}");
    }

    // Time the caller spends between rows is not charged to the budget.
    #[test]
    fn time_between_rows_is_not_charged() {
        let mut con = counting();
        con.set_query_budget(0.2).unwrap();
        con.execute("CREATE TABLE t(x INTEGER)", []).unwrap();
        con.executemany("INSERT INTO t VALUES(?)", (0..3).map(|i| [i]))
            .unwrap();
        let rows = con
            .query_rows("SELECT x FROM t ORDER BY x", [], |row| {
                std::thread::sleep(Duration::from_millis(150));
                Ok(row.get::<_, i64>(0)?)
            })
            .unwrap();
        assert_eq!(rows, vec![0, 1, 2]);
    }

    #[test]
    fn unbudgeted_connection_runs_freely() {
        let con = counting();
        assert_eq!(con.execute("CREATE TABLE t(x)", []).unwrap(), 0);
        let n: i64 = con
            .as_raw()
            .query_row("SELECT 7", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 7);
    }
}

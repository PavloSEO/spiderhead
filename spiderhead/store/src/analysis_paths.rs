//! Disk-backed shortest discovery paths over a caller-provided TEMP topology.
//!

use crate::error::StoreError;
use rusqlite::{params, Connection, OptionalExtension};

/// Regex source with Unicode matching.
pub const PREFIX: &str = r"[A-Za-z][A-Za-z0-9_]{0,48}\Z";

/// Validate a path-session prefix and return the quoted SQL identifier `"<prefix>_<suffix>"`.
pub fn name(prefix: &str, suffix: &str) -> Result<String, StoreError> {
    let mut chars = prefix.chars();
    let valid = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.clone().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && prefix.len() <= 49;
    if !valid {
        return Err(StoreError::Invalid(
            "path-session prefix must be a generated SQL identifier".into(),
        ));
    }
    Ok(format!("\"{prefix}_{suffix}\""))
}

/// A lazy FIFO BFS whose topology, queue, and predecessors stay in TEMP tables.
///
/// Call [`PathSession::close`] when done; the tables are not dropped automatically.
pub struct PathSession<'a> {
    con: &'a Connection,
    seed: String,
    topology: String,
    frontier: String,
    predecessor: String,
    topology_index: String,
    closed: bool,
}

impl<'a> PathSession<'a> {
    fn new(con: &'a Connection, prefix: &str, seed: &str) -> Result<Self, StoreError> {
        Ok(Self {
            con,
            seed: seed.to_owned(),
            topology: name(prefix, "topology")?,
            frontier: name(prefix, "path_frontier")?,
            predecessor: name(prefix, "path_predecessor")?,
            topology_index: format!("\"{prefix}_path_topology_src_seq\""),
            closed: false,
        })
    }

    /// Open a session for a validated TEMP topology, or return `None` when it is empty.
    pub fn open(
        con: &'a Connection,
        prefix: &str,
        seed: &str,
    ) -> Result<Option<PathSession<'a>>, StoreError> {
        if seed.is_empty() {
            return Err(StoreError::Invalid(
                "path session seed must be a normalized nonempty URL key".into(),
            ));
        }
        let topology = name(prefix, "topology")?;
        let columns: Vec<String> = {
            let mut stmt = con.prepare(&format!("PRAGMA table_info({topology})"))?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<Result<_, _>>()?
        };
        let has = |c: &str| columns.iter().any(|x| x == c);
        if !(has("seq") && has("src_key") && has("dst_key")) {
            return Err(StoreError::Invalid(
                "path session requires a TEMP topology with seq, src_key, dst_key".into(),
            ));
        }
        let count: i64 = con.query_row(&format!("SELECT COUNT(*) FROM {topology}"), [], |r| {
            r.get(0)
        })?;
        if count == 0 {
            return Ok(None);
        }
        let bad_row: Option<i64> = con
            .query_row(
                &format!(
                    "SELECT 1 FROM {topology} \
                     WHERE seq IS NULL OR src_key IS NULL OR dst_key IS NULL \
                     OR typeof(seq) != 'integer' OR typeof(src_key) != 'text' \
                     OR typeof(dst_key) != 'text' LIMIT 1"
                ),
                [],
                |r| r.get(0),
            )
            .optional()?;
        if bad_row.is_some() {
            return Err(StoreError::Invalid(
                "path topology contains an invalid edge row".into(),
            ));
        }
        let dup: Option<i64> = con
            .query_row(
                &format!("SELECT 1 FROM {topology} GROUP BY seq HAVING COUNT(*) != 1 LIMIT 1"),
                [],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            return Err(StoreError::Invalid(
                "path topology edge sequence must be unique".into(),
            ));
        }

        let session = Self::new(con, prefix, seed)?;
        con.execute_batch(&format!(
            "CREATE TEMP TABLE {frontier} (\
             key TEXT PRIMARY KEY, queue_order INTEGER NOT NULL UNIQUE, done INTEGER NOT NULL);\
             CREATE TEMP TABLE {predecessor} (\
             key TEXT PRIMARY KEY, parent_key TEXT, depth INTEGER NOT NULL);\
             CREATE INDEX \"{prefix}_path_pending\" ON {frontier}(queue_order) WHERE done=0;\
             CREATE INDEX {index} ON {topology}(src_key, seq);",
            frontier = session.frontier,
            predecessor = session.predecessor,
            index = session.topology_index,
        ))?;
        con.execute(
            &format!(
                "INSERT INTO {}(key, queue_order, done) VALUES(?1, 0, 0)",
                session.frontier
            ),
            params![seed],
        )?;
        con.execute(
            &format!(
                "INSERT INTO {}(key, parent_key, depth) VALUES(?1, NULL, 0)",
                session.predecessor
            ),
            params![seed],
        )?;
        Ok(Some(session))
    }

    /// Return the shortest path to one target, if reachable.
    pub fn path_to(&self, target: &str) -> Result<Option<Vec<String>>, StoreError> {
        if self.depth_for(target)?.is_some() {
            Ok(Some(self.materialize(target)?))
        } else {
            Ok(None)
        }
    }

    /// Resolve one distance without decoding its predecessor chain or all depths.
    pub fn depth_for(&self, target: &str) -> Result<Option<i64>, StoreError> {
        self.ensure_open()?;
        if target.is_empty() {
            return Err(StoreError::Invalid(
                "path target must be a normalized nonempty URL key".into(),
            ));
        }
        while !self.known(target)? {
            match self.next_pending()? {
                None => return Ok(None),
                Some(source) => self.expand(&source)?,
            }
        }
        let depth: i64 = self.con.query_row(
            &format!("SELECT depth FROM {} WHERE key=?1", self.predecessor),
            params![target],
            |r| r.get(0),
        )?;
        Ok(Some(depth))
    }

    /// Drain the frontier, then return `(key, hops)` for every reachable node.
    ///
    /// `path_to` expands only as far as one target, which is right when a check
    /// asks about a handful of pages and wrong when the question is the shape of
    /// the whole site. Exhausting the same FIFO walk gives the identical distances
    /// it would have reached one target at a time, and the predecessor rows stay
    /// materialized afterwards so a later `path_to` costs nothing.
    pub fn iter_depths(&self) -> Result<Vec<(String, i64)>, StoreError> {
        self.ensure_open()?;
        while let Some(source) = self.next_pending()? {
            self.expand(&source)?;
        }
        let mut stmt = self
            .con
            .prepare(&format!("SELECT key, depth FROM {}", self.predecessor))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn ensure_open(&self) -> Result<(), StoreError> {
        if self.closed {
            Err(StoreError::Invalid("path session is closed".into()))
        } else {
            Ok(())
        }
    }

    fn next_pending(&self) -> Result<Option<String>, StoreError> {
        let key: Option<String> = self
            .con
            .query_row(
                &format!(
                    "SELECT key FROM {} WHERE done=0 ORDER BY queue_order LIMIT 1",
                    self.frontier
                ),
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(key)
    }

    /// Whether `key` already has a predecessor row (it was discovered).
    pub fn known(&self, key: &str) -> Result<bool, StoreError> {
        let row: Option<i64> = self
            .con
            .query_row(
                &format!("SELECT 1 FROM {} WHERE key=?1", self.predecessor),
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(row.is_some())
    }

    /// Mark `source` expanded and enqueue its unseen out-neighbours in `seq` order.
    pub fn expand(&self, source: &str) -> Result<(), StoreError> {
        self.con.execute(
            &format!("UPDATE {} SET done=1 WHERE key=?1", self.frontier),
            params![source],
        )?;
        let depth: i64 = self.con.query_row(
            &format!("SELECT depth FROM {} WHERE key=?1", self.predecessor),
            params![source],
            |r| r.get(0),
        )?;
        let mut next_order: i64 = self.con.query_row(
            &format!(
                "SELECT COALESCE(MAX(queue_order)+1, 0) FROM {}",
                self.frontier
            ),
            [],
            |r| r.get(0),
        )?;
        // This query is the only edge materialization. FIFO source expansion,
        // then seq order, gives the same first-parent tie rule as a plain BFS.
        let destinations: Vec<String> = {
            let mut stmt = self.con.prepare(&format!(
                "SELECT dst_key FROM {} WHERE src_key=?1 ORDER BY seq",
                self.topology
            ))?;
            let rows = stmt.query_map(params![source], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for destination in destinations {
            let inserted = self.con.execute(
                &format!(
                    "INSERT OR IGNORE INTO {}(key, parent_key, depth) VALUES(?1, ?2, ?3)",
                    self.predecessor
                ),
                params![destination, source, depth + 1],
            )?;
            if inserted > 0 {
                self.con.execute(
                    &format!(
                        "INSERT INTO {}(key, queue_order, done) VALUES(?1, ?2, 0)",
                        self.frontier
                    ),
                    params![destination, next_order],
                )?;
                next_order += 1;
            }
        }
        Ok(())
    }

    /// Walk predecessor rows from `target` back to the seed and return the path in order.
    pub fn materialize(&self, target: &str) -> Result<Vec<String>, StoreError> {
        let mut path = Vec::new();
        let mut current: Option<String> = Some(target.to_owned());
        while let Some(key) = current {
            let parent: Option<Option<String>> = self
                .con
                .query_row(
                    &format!("SELECT parent_key FROM {} WHERE key=?1", self.predecessor),
                    params![key],
                    |r| r.get(0),
                )
                .optional()?;
            let parent = parent.ok_or_else(|| {
                StoreError::Invalid("path predecessor chain is inconsistent".into())
            })?;
            path.push(key);
            current = parent;
        }
        path.reverse();
        Ok(path)
    }

    /// Drop the session tables. Idempotent.
    pub fn close(&mut self) -> Result<(), StoreError> {
        if self.closed {
            return Ok(());
        }
        self.con.execute_batch(&format!(
            "DROP TABLE {frontier}; DROP TABLE {predecessor}; DROP INDEX {index};",
            frontier = self.frontier,
            predecessor = self.predecessor,
            index = self.topology_index,
        ))?;
        self.closed = true;
        Ok(())
    }

    /// The seed key this session starts from.
    pub fn seed(&self) -> &str {
        &self.seed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topology(con: &Connection, edges: &[(&str, &str)]) {
        con.execute_batch(
            "CREATE TEMP TABLE p_topology(seq INTEGER PRIMARY KEY, src_key TEXT, dst_key TEXT);",
        )
        .unwrap();
        for (seq, (s, d)) in edges.iter().enumerate() {
            con.execute(
                "INSERT INTO p_topology VALUES(?1,?2,?3)",
                params![seq as i64, s, d],
            )
            .unwrap();
        }
    }

    #[test]
    fn shortest_path_and_depth_follow_fifo_bfs() {
        let con = Connection::open_in_memory().unwrap();
        topology(
            &con,
            &[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d"), ("d", "a")],
        );
        let session = PathSession::open(&con, "p", "a").unwrap().unwrap();
        assert_eq!(session.depth_for("d").unwrap(), Some(2));
        assert_eq!(session.path_to("d").unwrap().unwrap(), vec!["a", "b", "d"]);
        assert_eq!(session.depth_for("zzz").unwrap(), None);
    }

    #[test]
    fn empty_topology_has_no_session() {
        let con = Connection::open_in_memory().unwrap();
        topology(&con, &[]);
        assert!(PathSession::open(&con, "p", "a").unwrap().is_none());
    }

    #[test]
    fn prefix_is_validated() {
        assert!(name("bad-prefix", "x").is_err());
        assert_eq!(name("ok_1", "topology").unwrap(), "\"ok_1_topology\"");
    }
}

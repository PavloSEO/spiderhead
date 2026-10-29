//! Disk-backed PageRank calculation for an already prepared native graph.
//!
//!
//! The caller owns the temporary node and topology tables. This module only creates short-lived
//! calculation tables with a caller-provided private prefix; it neither knows crawler rows nor
//! depends on any check policy.

use crate::error::StoreError;
use rusqlite::{params, Connection, OptionalExtension};

/// Read scores without materializing the node set in memory.
///
/// Borrows the connection: the view lives only while the caller holds the same connection.
pub struct ScoreView<'a> {
    /// Connection that owns the TEMP score table.
    pub con: &'a Connection,
    /// Name of the TEMP score table (`<prefix>_score`).
    pub table: String,
    /// Number of nodes that were scored.
    pub count: i64,
    /// Median score (average of the middle one or two values).
    pub median: f64,
}

impl<'a> ScoreView<'a> {
    /// Score of one node, or `None` when the node is absent from the score table.
    pub fn score_for(&self, key: &str) -> Result<Option<f64>, StoreError> {
        let sql = format!("SELECT score FROM {} WHERE node=?1", self.table);
        let row: Option<f64> = self
            .con
            .query_row(&sql, params![key], |r| r.get::<_, f64>(0))
            .optional()?;
        Ok(row)
    }

    /// Release the score table after its consumer has emitted bounded output.
    pub fn close(&self) -> Result<(), StoreError> {
        self.con
            .execute(&format!("DROP TABLE IF EXISTS temp.{}", self.table), [])?;
        Ok(())
    }
}

/// Validate a generated SQLite identifier (`[A-Za-z_][A-Za-z0-9_]*`).
pub fn name(value: &str, label: &str) -> Result<String, StoreError> {
    let mut chars = value.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    if first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(value.to_owned())
    } else {
        Err(StoreError::Invalid(format!(
            "{label} must be a generated SQLite identifier"
        )))
    }
}

/// Require that `name` names an existing TEMP table.
pub fn temp_table(con: &Connection, name: &str, label: &str) -> Result<(), StoreError> {
    let found: Option<i64> = con
        .query_row(
            "SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name=?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    if found.is_none() {
        return Err(StoreError::Invalid(format!(
            "{label} must name an existing TEMP table"
        )));
    }
    Ok(())
}

/// Check damping in `[0, 1]`, a positive iteration limit and a finite non-negative tolerance.
pub fn validate_parameters(
    damping: f64,
    max_iterations: i64,
    tolerance: f64,
) -> Result<(), StoreError> {
    let ok = max_iterations >= 1
        && damping.is_finite()
        && (0.0..=1.0).contains(&damping)
        && tolerance.is_finite()
        && tolerance >= 0.0;
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "invalid PageRank damping, iteration limit, or tolerance".into(),
        ))
    }
}

/// Calculate the existing PageRank model using TEMP tables only.
///
/// ``nodes_table`` has ``node TEXT PRIMARY KEY`` and includes current pages
/// plus every followed internal edge endpoint. ``topology_table`` has
/// ``seq INTEGER PRIMARY KEY, src_key TEXT, dst_key TEXT`` and contains only
/// followed internal links. A nonempty self-edge topology is intentional:
/// it creates a valid dangling-node graph; self links are ignored for distribution.
pub fn compute_scores<'a>(
    con: &'a Connection,
    nodes_table: &str,
    topology_table: &str,
    prefix: &str,
    damping: f64,
    max_iterations: i64,
    tolerance: f64,
) -> Result<Option<ScoreView<'a>>, StoreError> {
    let nodes_table = name(nodes_table, "nodes_table")?;
    let topology_table = name(topology_table, "topology_table")?;
    let prefix = name(prefix, "prefix")?;
    validate_parameters(damping, max_iterations, tolerance)?;
    temp_table(con, &nodes_table, "nodes_table")?;
    temp_table(con, &topology_table, "topology_table")?;

    let score = format!("{prefix}_score");
    let next_score = format!("{prefix}_next");
    let outdegree = format!("{prefix}_outdegree");
    let contribution = format!("{prefix}_contribution");
    let index_source = format!("{prefix}_topology_source");
    let index_dest = format!("{prefix}_topology_dest");
    for owned in [
        &score,
        &next_score,
        &outdegree,
        &contribution,
        &index_source,
        &index_dest,
    ] {
        let taken: Option<i64> = con
            .query_row(
                "SELECT 1 FROM sqlite_temp_master WHERE name=?1 COLLATE NOCASE",
                params![owned],
                |r| r.get(0),
            )
            .optional()?;
        if taken.is_some() {
            return Err(StoreError::Invalid(
                "score prefix already has temporary state".into(),
            ));
        }
    }

    let run = || -> Result<Option<ScoreView<'a>>, StoreError> {
        let has_edge: Option<i64> = con
            .query_row(
                &format!("SELECT 1 FROM {topology_table} LIMIT 1"),
                [],
                |r| r.get(0),
            )
            .optional()?;
        if has_edge.is_none() {
            return Ok(None);
        }
        let dangling_endpoint: Option<i64> = con
            .query_row(
                &format!(
                    "SELECT 1 FROM {topology_table} AS edge \
                     LEFT JOIN {nodes_table} AS source ON source.node=edge.src_key \
                     LEFT JOIN {nodes_table} AS destination ON destination.node=edge.dst_key \
                     WHERE source.node IS NULL OR destination.node IS NULL LIMIT 1"
                ),
                [],
                |r| r.get(0),
            )
            .optional()?;
        if dangling_endpoint.is_some() {
            return Err(StoreError::Invalid(
                "topology endpoint is absent from nodes_table".into(),
            ));
        }
        let count: i64 =
            con.query_row(&format!("SELECT COUNT(*) FROM {nodes_table}"), [], |r| {
                r.get(0)
            })?;
        if count == 0 {
            return Ok(None);
        }

        con.execute_batch(&format!(
            "CREATE TEMP TABLE {score}(node TEXT PRIMARY KEY,score REAL NOT NULL) WITHOUT ROWID;
             CREATE TEMP TABLE {next_score}(node TEXT PRIMARY KEY,score REAL NOT NULL) WITHOUT ROWID;
             CREATE TEMP TABLE {outdegree}(node TEXT PRIMARY KEY,degree INTEGER NOT NULL) WITHOUT ROWID;
             CREATE TEMP TABLE {contribution}(node TEXT PRIMARY KEY,score REAL NOT NULL) WITHOUT ROWID;
             CREATE INDEX temp.{index_source} ON {topology_table}(src_key,seq);
             CREATE INDEX temp.{index_dest} ON {topology_table}(dst_key,seq);"
        ))?;
        con.execute(
            &format!(
                "INSERT INTO {outdegree} \
                 SELECT src_key,COUNT(*) FROM {topology_table} \
                 WHERE src_key<>dst_key GROUP BY src_key"
            ),
            [],
        )?;
        con.execute(
            &format!("INSERT INTO {score} SELECT node,?1 FROM {nodes_table}"),
            params![1.0 / count as f64],
        )?;

        for _ in 0..max_iterations {
            let dangling: f64 = con.query_row(
                &format!(
                    "SELECT COALESCE(SUM(score.score),0) FROM {score} AS score \
                     LEFT JOIN {outdegree} AS degree ON degree.node=score.node \
                     WHERE degree.degree IS NULL"
                ),
                [],
                |r| r.get(0),
            )?;
            let base = (1.0 - damping) / count as f64 + damping * dangling / count as f64;
            con.execute(&format!("DELETE FROM {next_score}"), [])?;
            con.execute(
                &format!("INSERT INTO {next_score} SELECT node,?1 FROM {nodes_table}"),
                params![base],
            )?;
            con.execute(&format!("DELETE FROM {contribution}"), [])?;
            con.execute(
                &format!(
                    "INSERT INTO {contribution} \
                     SELECT edge.dst_key,SUM(?1*score.score/degree.degree) \
                     FROM {topology_table} AS edge \
                     JOIN {score} AS score ON score.node=edge.src_key \
                     JOIN {outdegree} AS degree ON degree.node=edge.src_key \
                     WHERE edge.src_key<>edge.dst_key GROUP BY edge.dst_key"
                ),
                params![damping],
            )?;
            con.execute(
                &format!(
                    "UPDATE {next_score} SET score=score+COALESCE(\
                     (SELECT contribution.score FROM {contribution} AS contribution \
                     WHERE contribution.node={next_score}.node),0)"
                ),
                [],
            )?;
            let delta: Option<f64> = con.query_row(
                &format!(
                    "SELECT MAX(ABS(next.score-current.score)) FROM {next_score} AS next \
                     JOIN {score} AS current USING(node)"
                ),
                [],
                |r| r.get(0),
            )?;
            let delta = delta.ok_or_else(|| {
                StoreError::Invalid("PageRank iteration produced no delta".into())
            })?;
            con.execute(&format!("DELETE FROM {score}"), [])?;
            con.execute(
                &format!("INSERT INTO {score} SELECT node,score FROM {next_score}"),
                [],
            )?;
            if delta < tolerance {
                break;
            }
        }

        let offset = (count - 1) / 2;
        let width = if count % 2 == 1 { 1 } else { 2 };
        let median: f64 = con.query_row(
            &format!(
                "SELECT AVG(score) FROM (SELECT score FROM {score} ORDER BY score,node LIMIT ?1 OFFSET ?2)"
            ),
            params![width, offset],
            |r| r.get(0),
        )?;
        for table in [&next_score, &outdegree, &contribution] {
            con.execute(&format!("DROP TABLE temp.{table}"), [])?;
        }
        for index in [&index_source, &index_dest] {
            con.execute(&format!("DROP INDEX temp.{index}"), [])?;
        }
        Ok(Some(ScoreView {
            con,
            table: score.clone(),
            count,
            median,
        }))
    };

    match run() {
        Ok(view) => Ok(view),
        Err(err) => {
            // Cleanup runs on any failure, then the original error is re-raised.
            for table in [&next_score, &outdegree, &contribution, &score] {
                let _ = con.execute(&format!("DROP TABLE IF EXISTS temp.{table}"), []);
            }
            for index in [&index_source, &index_dest] {
                let _ = con.execute(&format!("DROP INDEX IF EXISTS temp.{index}"), []);
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_topology_is_unavailable() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TEMP TABLE nodes(node TEXT PRIMARY KEY);
             INSERT INTO nodes VALUES('a');
             CREATE TEMP TABLE topo(seq INTEGER PRIMARY KEY, src_key TEXT, dst_key TEXT);",
        )
        .unwrap();
        assert!(compute_scores(&con, "nodes", "topo", "s1", 0.85, 50, 1e-9)
            .unwrap()
            .is_none());
    }

    #[test]
    fn scores_sum_to_one_on_a_cycle() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TEMP TABLE nodes(node TEXT PRIMARY KEY);
             INSERT INTO nodes VALUES('a'),('b'),('c');
             CREATE TEMP TABLE topo(seq INTEGER PRIMARY KEY, src_key TEXT, dst_key TEXT);
             INSERT INTO topo VALUES(0,'a','b'),(1,'b','c'),(2,'c','a');",
        )
        .unwrap();
        let view = compute_scores(&con, "nodes", "topo", "s2", 0.85, 100, 1e-12)
            .unwrap()
            .unwrap();
        assert_eq!(view.count, 3);
        let a = view.score_for("a").unwrap().unwrap();
        assert!((a - 1.0 / 3.0).abs() < 1e-9);
        assert!((view.median - 1.0 / 3.0).abs() < 1e-9);
        view.close().unwrap();
    }

    // regression: a prefix colliding only in case must be refused before CREATE,
    // and the failure cleanup must not drop the foreign TEMP table
    #[test]
    fn case_only_prefix_collision_refuses_and_preserves_foreign_table() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TEMP TABLE nodes(node TEXT PRIMARY KEY);
             INSERT INTO nodes VALUES('a');
             CREATE TEMP TABLE topo(seq INTEGER PRIMARY KEY, src_key TEXT, dst_key TEXT);
             INSERT INTO topo VALUES(0,'a','a');
             CREATE TEMP TABLE S3_SCORE(node TEXT PRIMARY KEY, score REAL NOT NULL) WITHOUT ROWID;
             INSERT INTO S3_SCORE VALUES('a', 1.0);",
        )
        .unwrap();
        assert!(compute_scores(&con, "nodes", "topo", "s3", 0.85, 50, 1e-9).is_err());
        let still_there: i64 = con
            .query_row("SELECT COUNT(*) FROM S3_SCORE", [], |r| r.get(0))
            .unwrap();
        assert_eq!(still_there, 1);
    }

    #[test]
    fn rejects_bad_parameters_and_identifiers() {
        assert!(name("a b", "nodes").is_err());
        assert!(validate_parameters(1.5, 10, 0.0).is_err());
        assert!(validate_parameters(0.85, 0, 0.0).is_err());
        assert!(validate_parameters(0.85, 10, 0.0).is_ok());
    }
}

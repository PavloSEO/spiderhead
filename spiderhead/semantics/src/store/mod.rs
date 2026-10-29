//! SQLite store of one semantic core: a single database file per
//! project, WAL mode, with a fixed schema.

use indexmap::IndexMap;
use rusqlite::{params_from_iter, Connection, OpenFlags};
use serde_json::{json, Value};
use std::cell::Cell;
use std::path::{Path, PathBuf};

use crate::pyfmt::dumps;

mod store_assign_cluster;
pub use store_assign_cluster::*;

/// Local wall clock as `YYYY-MM-DD HH:MM:SS`
/// (via `localtime_r` / `localtime_s`).
pub fn local_seconds() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        #[cfg(unix)]
        let ok = !libc::localtime_r(&now, &mut tm).is_null();
        #[cfg(windows)]
        let ok = libc::localtime_s(&mut tm, &now) == 0;
        if !ok {
            return "1970-01-01 00:00:00".to_string();
        }
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// UTC instant as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn zulu_seconds() -> String {
    spiderhead_core::timeutil::stamp(spiderhead_core::timeutil::Format::ZuluSeconds)
}

/// Database schema; its text is fixed for existing databases.
pub const SCHEMA: &str = "\
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
CREATE TABLE IF NOT EXISTS phrases(
  id INTEGER PRIMARY KEY,
  norm TEXT NOT NULL UNIQUE,
  base INTEGER, quoted INTEGER, exact INTEGER,  -- W (broad), \"W\" (phrase), !W (exact)
  impr INTEGER,                     -- observed impressions from an import, else NULL
  pos_y INTEGER, pos_g INTEGER,     -- average Yandex/Google position (1..100; NULL = not in top 100)
  region TEXT DEFAULT '225',
  depth INTEGER DEFAULT 0,
  src TEXT,
  status TEXT DEFAULT 'new',        -- new|kept|review|dropped|dead
  reason TEXT,                      -- why the phrase has this status
  reason_stage TEXT,               -- which stage set the status
  route TEXT DEFAULT 'commercial',  -- commercial|info|nav
  page_types TEXT,                  -- comma-separated page types (landing, blog, glossary)
  lemma_group INTEGER,
  cluster_id INTEGER,
  first_seen TEXT, last_update TEXT
);
CREATE INDEX IF NOT EXISTS ix_phrases_status ON phrases(status);
CREATE INDEX IF NOT EXISTS ix_phrases_base ON phrases(base);
CREATE TABLE IF NOT EXISTS expansions(
  seed_norm TEXT PRIMARY KEY, expanded_at TEXT,
  results_n INTEGER, assoc_n INTEGER, region TEXT
);
CREATE TABLE IF NOT EXISTS edges(
  parent TEXT, child TEXT, kind TEXT
);
CREATE INDEX IF NOT EXISTS ix_edges_child ON edges(child);
CREATE TABLE IF NOT EXISTS serp(
  phrase_norm TEXT, pos INTEGER, url TEXT, domain TEXT, title TEXT, fetched_at TEXT
);
CREATE INDEX IF NOT EXISTS ix_serp_phrase ON serp(phrase_norm);
CREATE TABLE IF NOT EXISTS clusters(
  id INTEGER PRIMARY KEY, name TEXT, marker_norm TEXT,
  coherence REAL, is_real INTEGER, note TEXT,
  landing_url TEXT,                 -- landing page of the cluster
  intent TEXT,                      -- commercial|info|nav
  source TEXT,                      -- manual_sya|serp|louvain|competitors
  page_types TEXT                   -- comma-separated page types
);
CREATE INDEX IF NOT EXISTS ix_phrases_cluster ON phrases(cluster_id);
CREATE TABLE IF NOT EXISTS runs(
  id INTEGER PRIMARY KEY, ts TEXT, stage TEXT,
  n_requests INTEGER, cost_rub REAL, cost_arsenkin INTEGER, note TEXT
);
CREATE INDEX IF NOT EXISTS ix_serp_domain ON serp(domain);
-- Additive workflow tables: the original phrase/run tables remain format-compatible.
CREATE TABLE IF NOT EXISTS semantic_stage_runs(
  id INTEGER PRIMARY KEY, stage TEXT NOT NULL, state TEXT NOT NULL,
  started_at TEXT NOT NULL, finished_at TEXT, estimate_json TEXT,
  result_json TEXT, error TEXT
);
CREATE INDEX IF NOT EXISTS ix_semantic_stage_runs ON semantic_stage_runs(stage, id DESC);
CREATE TABLE IF NOT EXISTS semantic_audit(
  id INTEGER PRIMARY KEY, at TEXT NOT NULL, action TEXT NOT NULL,
  entity TEXT NOT NULL, before_json TEXT, after_json TEXT, evidence_json TEXT
);
CREATE TABLE IF NOT EXISTS landing_candidates(
  cluster_id INTEGER NOT NULL, url TEXT NOT NULL, score REAL,
  evidence_json TEXT NOT NULL, computed_at TEXT NOT NULL,
  PRIMARY KEY(cluster_id, url)
);
-- Competitor analysis: who appears in the core's SERP and how often (refresh_domains()).
CREATE TABLE IF NOT EXISTS domains(
  domain TEXT PRIMARY KEY,
  n_queries INTEGER,        -- how many core queries show this domain
  avg_pos REAL, best_pos INTEGER,
  kind TEXT                 -- own | competitor | hub
);
-- Readable views for a database browser, no joins needed.
CREATE VIEW IF NOT EXISTS v_core AS
  SELECT p.norm AS phrase, p.base, p.quoted, p.exact, p.pos_y AS yandex_pos, p.pos_g AS google_pos,
         p.page_types, p.route AS intent, c.name AS cluster, c.landing_url AS landing
  FROM phrases p LEFT JOIN clusters c ON c.id=p.cluster_id
  WHERE p.status IN('kept','review') AND p.page_types IS NOT NULL;
CREATE VIEW IF NOT EXISTS v_competitors AS
  SELECT domain AS competitor, n_queries AS queries, ROUND(avg_pos,1) AS avg_pos, best_pos
  FROM domains WHERE kind='competitor' ORDER BY n_queries DESC;
CREATE VIEW IF NOT EXISTS v_clusters AS
  SELECT c.id, c.name AS cluster, COUNT(p.id) AS phrases, ROUND(c.coherence,2) AS coherence,
         c.page_types, c.landing_url AS landing, c.source
  FROM clusters c LEFT JOIN phrases p ON p.cluster_id=c.id AND p.status IN('kept','review')
  GROUP BY c.id;
";

/// A result row as an insertion-ordered column map
/// that keeps the SELECT column order.
pub type Row = IndexMap<String, Value>;

fn row_value(row: &rusqlite::Row, i: usize) -> Value {
    use rusqlite::types::ValueRef;
    match row.get_ref(i) {
        Ok(ValueRef::Null) => Value::Null,
        Ok(ValueRef::Integer(v)) => json!(v),
        Ok(ValueRef::Real(v)) => json!(v),
        Ok(ValueRef::Text(v)) => json!(String::from_utf8_lossy(v)),
        Ok(ValueRef::Blob(v)) => json!(format!("{v:?}")),
        Err(_) => Value::Null,
    }
}

/// The `Store` — owns the `rusqlite::Connection` plus the path.
pub struct Store {
    /// Database path as a string (`self.path`).
    pub path: String,
    /// Live connection; stage code queries through it.
    pub conn: Connection,
    /// Whether an implicit transaction is open: the connection runs
    /// `BEGIN` before the first DML statement and
    /// keeps it open until `commit()`/`rollback()`. The driver defaults to
    /// autocommit, so the stage receipts/rollback semantics need this flag.
    tx_open: Cell<bool>,
}

impl Store {
    /// Open or create the store at `db_path`: create the directory, run the schema,
    /// idempotent migrations. `readonly` refuses missing files.
    pub fn open(db_path: &Path, readonly: bool) -> Result<Self, crate::Error> {
        let path = db_path.to_string_lossy().into_owned();
        let conn = if readonly {
            let abs = if db_path.is_absolute() {
                db_path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|e| crate::Error::Io(e.to_string()))?
                    .join(db_path)
            };
            if !abs.exists() {
                return Err(crate::Error::Io(format!(
                    "unable to open database file: {}",
                    abs.display()
                )));
            }
            let c = Connection::open_with_flags(&abs, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(crate::Error::Db)?;
            c.execute_batch("PRAGMA query_only=ON")
                .map_err(crate::Error::Db)?;
            c
        } else {
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| crate::Error::Io(format!("{e}: {}", parent.display())))?;
            }
            let c = Connection::open(db_path).map_err(crate::Error::Db)?;
            c.execute_batch(SCHEMA).map_err(crate::Error::Db)?;
            let s = Self {
                path: path.clone(),
                conn: c,
                tx_open: Cell::new(false),
            };
            s.migrate()?;
            return Ok(s);
        };
        Ok(Store {
            path,
            conn,
            tx_open: Cell::new(false),
        })
    }

    /// In-memory store for tests.
    pub fn open_memory() -> Result<Self, crate::Error> {
        let c = Connection::open_in_memory().map_err(crate::Error::Db)?;
        c.execute_batch(SCHEMA).map_err(crate::Error::Db)?;
        let s = Self {
            path: ":memory:".into(),
            conn: c,
            tx_open: Cell::new(false),
        };
        s.migrate()?;
        Ok(s)
    }

    /// Idempotent `ALTER TABLE ADD COLUMN` for older databases.
    fn migrate(&self) -> Result<(), crate::Error> {
        let adds: &[(&str, &str, &str)] = &[
            ("phrases", "impr", "INTEGER"),
            ("phrases", "pos_y", "INTEGER"),
            ("phrases", "pos_g", "INTEGER"),
            ("phrases", "pos_serp", "INTEGER"),
            ("phrases", "page_types", "TEXT"),
            ("phrases", "cluster_soft", "INTEGER"),
            ("phrases", "cluster_mid", "INTEGER"),
            ("phrases", "cluster_hard", "INTEGER"),
            ("clusters", "landing_url", "TEXT"),
            ("clusters", "intent", "TEXT"),
            ("clusters", "source", "TEXT"),
            ("clusters", "level", "TEXT"),
            ("clusters", "page_types", "TEXT"),
            ("phrases", "group_name", "TEXT"),
            ("phrases", "subgroup", "TEXT"),
            ("phrases", "url", "TEXT"),
            ("phrases", "comment", "TEXT"),
        ];
        for (table, col, typ) in adds {
            let have = self.columns(table)?;
            if !have.iter().any(|c| c == col) {
                self.conn
                    .execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {typ}"), [])
                    .map_err(crate::Error::Db)?;
            }
        }
        Ok(())
    }

    /// Column names of a table (`PRAGMA table_info`), in declared order.
    pub fn columns(&self, table: &str) -> Result<Vec<String>, crate::Error> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(crate::Error::Db)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(crate::Error::Db)?;
        let mut out = Vec::new();
        for c in rows.flatten() {
            out.push(c);
        }
        Ok(out)
    }

    /// All rows of a query as column maps.
    pub fn q(&self, sql: &str, args: &[Value]) -> Result<Vec<Row>, crate::Error> {
        let mut stmt = self.conn.prepare(sql).map_err(crate::Error::Db)?;
        let names: Vec<String> = stmt
            .column_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        let params: Vec<rusqlite::types::Value> = args.iter().map(to_sql).collect();
        let mapped = stmt
            .query_map(params_from_iter(params), |r| {
                let mut row = Row::new();
                for (i, name) in names.iter().enumerate() {
                    row.insert(name.clone(), row_value(r, i));
                }
                Ok(row)
            })
            .map_err(crate::Error::Db)?;
        let mut out = Vec::new();
        for r in mapped {
            out.push(r.map_err(crate::Error::Db)?);
        }
        Ok(out)
    }

    /// First row of a query, or `None`.
    pub fn q1(&self, sql: &str, args: &[Value]) -> Result<Option<Row>, crate::Error> {
        Ok(self.q(sql, args)?.into_iter().next())
    }

    /// Implicit `BEGIN` before the first DML statement.
    /// DDL (CREATE/ALTER) stays in autocommit.
    fn begin_if_dml(&self, sql: &str) -> Result<(), crate::Error> {
        let word: String = sql
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_uppercase();
        if matches!(word.as_str(), "INSERT" | "UPDATE" | "DELETE" | "REPLACE")
            && !self.tx_open.get()
        {
            self.conn.execute_batch("BEGIN").map_err(crate::Error::Db)?;
            self.tx_open.set(true);
        }
        Ok(())
    }

    /// Run an INSERT, UPDATE or DELETE and return the row count.
    pub fn exec(&self, sql: &str, args: &[Value]) -> Result<usize, crate::Error> {
        self.begin_if_dml(sql)?;
        let params: Vec<rusqlite::types::Value> = args.iter().map(to_sql).collect();
        self.conn
            .execute(sql, params_from_iter(params))
            .map_err(crate::Error::Db)
    }

    /// Commit; a no-op when no transaction is open.
    pub fn commit(&self) -> Result<(), crate::Error> {
        if self.tx_open.get() {
            self.conn
                .execute_batch("COMMIT")
                .map_err(crate::Error::Db)?;
            self.tx_open.set(false);
        }
        Ok(())
    }

    /// Commit variant that swallows driver errors (still clears the flag).
    pub fn commit_soft(&self) {
        if self.tx_open.get() {
            let _ = self.conn.execute_batch("COMMIT");
            self.tx_open.set(false);
        }
    }

    /// Roll back every write since the last commit.
    pub fn rollback(&self) -> Result<(), crate::Error> {
        if self.tx_open.get() {
            self.conn
                .execute_batch("ROLLBACK")
                .map_err(crate::Error::Db)?;
            self.tx_open.set(false);
        }
        Ok(())
    }

    // ---------- phrases ----------

    /// Insert a phrase or keep the highest base; true for a new phrase.
    pub fn upsert(
        &self,
        norm: &str,
        base: Option<i64>,
        depth: i64,
        src: Option<&str>,
        region: &str,
    ) -> Result<bool, crate::Error> {
        let row = self.q1("SELECT id, base FROM phrases WHERE norm=?", &[json!(norm)])?;
        let now = local_seconds();
        match row {
            None => {
                self.exec(
                    "INSERT INTO phrases(norm,base,depth,src,region,status,first_seen,last_update) \
                     VALUES(?,?,?,?,?, 'new', ?, ?)",
                    &[json!(norm), json!(base), json!(depth), json!(src), json!(region), json!(now), json!(now)],
                )?;
                Ok(true)
            }
            Some(r) => {
                let cur = r.get("base").and_then(|v| v.as_i64());
                if let Some(b) = base {
                    if cur.is_none_or(|c| b > c) {
                        self.exec(
                            "UPDATE phrases SET base=?, last_update=? WHERE id=?",
                            &[json!(b), json!(now), r["id"].clone()],
                        )?;
                    }
                }
                Ok(false)
            }
        }
    }

    /// Set named columns of a phrase and update `last_update`.
    pub fn set_fields(&self, norm: &str, kw: &[(String, Value)]) -> Result<(), crate::Error> {
        if kw.is_empty() {
            return Ok(());
        }
        let mut cols = String::new();
        let mut args: Vec<Value> = Vec::new();
        for (k, v) in kw {
            if !cols.is_empty() {
                cols.push_str(", ");
            }
            cols.push_str(&format!("{k}=?"));
            args.push(v.clone());
        }
        cols.push_str(", last_update=?");
        args.push(json!(local_seconds()));
        args.push(json!(norm));
        self.exec(&format!("UPDATE phrases SET {cols} WHERE norm=?"), &args)?;
        Ok(())
    }

    /// Set status, reason and the deciding stage.
    pub fn set_status(
        &self,
        norm: &str,
        status: &str,
        reason: Option<&str>,
        stage: Option<&str>,
    ) -> Result<(), crate::Error> {
        self.exec(
            "UPDATE phrases SET status=?, reason=?, reason_stage=?, last_update=? WHERE norm=?",
            &[
                json!(status),
                json!(reason),
                json!(stage),
                json!(local_seconds()),
                json!(norm),
            ],
        )?;
        Ok(())
    }

    /// Add an edge between two phrases.
    pub fn add_edge(&self, parent: &str, child: &str, kind: &str) -> Result<(), crate::Error> {
        self.exec(
            "INSERT INTO edges(parent,child,kind) VALUES(?,?,?)",
            &[json!(parent), json!(child), json!(kind)],
        )?;
        Ok(())
    }

    /// Find or create a cluster by name; returns its id.
    pub fn upsert_cluster(
        &self,
        name: &str,
        source: Option<&str>,
        intent: Option<&str>,
        landing_url: Option<&str>,
    ) -> Result<i64, crate::Error> {
        if let Some(r) = self.q1("SELECT id FROM clusters WHERE name=?", &[json!(name)])? {
            return Ok(r["id"].as_i64().unwrap_or(0));
        }
        self.exec(
            "INSERT INTO clusters(name,source,intent,landing_url) VALUES(?,?,?,?)",
            &[
                json!(name),
                json!(source),
                json!(intent),
                json!(landing_url),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
}

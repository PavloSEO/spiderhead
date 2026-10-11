"""SQLite store of one semantic core: a single database file per project, WAL mode.

The store accumulates: phrases are deduplicated by ``phrases.norm``; ``expansions`` records
seeds that were already expanded so they are never paid for twice; ``edges`` keeps the
provenance graph; ``runs`` keeps per-stage provider units. Money is never estimated here: the
shared ``seohead.data_sources.spend`` journal is the record of what providers charged.
"""

from __future__ import annotations

import csv
import json
import sqlite3
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = """
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
CREATE TABLE IF NOT EXISTS phrases(
  id INTEGER PRIMARY KEY,
  norm TEXT NOT NULL UNIQUE,
  base INTEGER, quoted INTEGER, exact INTEGER,  -- W (broad), "W" (phrase), !W (exact)
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
"""


def _now():
    return time.strftime("%Y-%m-%d %H:%M:%S")


_DEFAULT_FETCH_TIME = object()


class Store:
    def __init__(self, db_path, readonly=False):
        self.path = str(db_path)
        if readonly:
            uri = Path(self.path).resolve().as_uri() + "?mode=ro"
            self.db = sqlite3.connect(uri, uri=True)
        else:
            Path(self.path).parent.mkdir(parents=True, exist_ok=True)
            self.db = sqlite3.connect(self.path)
        self.db.row_factory = sqlite3.Row
        if readonly:
            self.db.execute("PRAGMA query_only=ON")
        else:
            self.db.executescript(SCHEMA)
            self._migrate()
            self.db.commit()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        if exc_type is None:
            self.db.commit()
        else:
            self.db.rollback()
        self.db.close()

    def _migrate(self):
        """Idempotent ALTERs for databases created before the schema grew."""
        adds = [
            ("phrases", "impr", "INTEGER"),
            ("phrases", "pos_y", "INTEGER"),
            ("phrases", "pos_g", "INTEGER"),
            ("phrases", "pos_serp", "INTEGER"),  # own-domain position in the collected SERP
            ("phrases", "page_types", "TEXT"),
            ("phrases", "cluster_soft", "INTEGER"),  # level 1: broad topics
            ("phrases", "cluster_mid", "INTEGER"),  # level 2: medium topics
            ("phrases", "cluster_hard", "INTEGER"),  # level 3: narrow topics
            ("clusters", "landing_url", "TEXT"),
            ("clusters", "intent", "TEXT"),
            ("clusters", "source", "TEXT"),
            ("clusters", "level", "TEXT"),  # soft|mid|hard
            ("clusters", "page_types", "TEXT"),
            ("phrases", "group_name", "TEXT"),
            ("phrases", "subgroup", "TEXT"),
            ("phrases", "url", "TEXT"),
            ("phrases", "comment", "TEXT"),
        ]
        for table, col, typ in adds:
            have = [r["name"] for r in self.db.execute("PRAGMA table_info(" + table + ")")]
            if col not in have:
                self.db.execute("ALTER TABLE " + table + " ADD COLUMN " + col + " " + typ)
        self.db.commit()

    def close(self):
        self.db.commit()
        self.db.close()

    # ---------- phrases ----------
    def upsert(self, norm, base=None, depth=0, src=None, region="225"):
        """Insert or update a phrase, keeping the highest base. Return True for a new phrase."""
        cur = self.db.execute("SELECT id, base FROM phrases WHERE norm=?", (norm,))
        row = cur.fetchone()
        if row is None:
            self.db.execute(
                "INSERT INTO phrases(norm,base,depth,src,region,status,first_seen,last_update)"
                " VALUES(?,?,?,?,?, 'new', ?, ?)",
                (norm, base, depth, src, region, _now(), _now()),
            )
            return True
        if base is not None and (row["base"] is None or base > row["base"]):
            self.db.execute(
                "UPDATE phrases SET base=?, last_update=? WHERE id=?", (base, _now(), row["id"])
            )
        return False

    def set_fields(self, norm, **kw):
        if not kw:
            return
        kw["last_update"] = _now()
        cols = ", ".join(f"{k}=?" for k in kw)
        self.db.execute(f"UPDATE phrases SET {cols} WHERE norm=?", (*kw.values(), norm))

    def set_status(self, norm, status, reason=None, stage=None):
        """Change a status together with the reason and the stage that decided it."""
        self.db.execute(
            "UPDATE phrases SET status=?, reason=?, reason_stage=?, last_update=? WHERE norm=?",
            (status, reason, stage, _now(), norm),
        )

    def add_edge(self, parent, child, kind):
        self.db.execute("INSERT INTO edges(parent,child,kind) VALUES(?,?,?)", (parent, child, kind))

    # ---------- clusters: query -> cluster -> landing page ----------
    def upsert_cluster(self, name, source=None, intent=None, landing_url=None):
        """Find or create a cluster by name and return its id."""
        r = self.db.execute("SELECT id FROM clusters WHERE name=?", (name,)).fetchone()
        if r:
            return r["id"]
        cur = self.db.execute(
            "INSERT INTO clusters(name,source,intent,landing_url) VALUES(?,?,?,?)",
            (name, source, intent, landing_url),
        )
        return cur.lastrowid

    def assign_cluster(self, norm, cluster_id):
        self.db.execute(
            "UPDATE phrases SET cluster_id=?, last_update=? WHERE norm=?",
            (cluster_id, _now(), norm),
        )

    def set_landing(self, cluster_id, url):
        """Set a cluster landing and retain a local audit record."""
        before = self.db.execute(
            "SELECT landing_url FROM clusters WHERE id=?", (cluster_id,)
        ).fetchone()
        if before is None:
            raise ValueError(f"unknown cluster id: {cluster_id}")
        self.db.execute("UPDATE clusters SET landing_url=? WHERE id=?", (url, cluster_id))
        self.db.execute(
            "INSERT INTO semantic_audit(at,action,entity,before_json,after_json) VALUES(?,?,?,?,?)",
            (
                datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
                "set_landing",
                f"cluster:{cluster_id}",
                json.dumps({"landing_url": before[0]}, ensure_ascii=False),
                json.dumps({"landing_url": url}, ensure_ascii=False),
            ),
        )

    def start_stage(self, stage, estimate=None):
        now = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
        cur = self.db.execute(
            "INSERT INTO semantic_stage_runs(stage,state,started_at,estimate_json) "
            "VALUES(?,'running',?,?)",
            (
                stage,
                now,
                json.dumps(estimate, ensure_ascii=False, default=str)
                if estimate is not None
                else None,
            ),
        )
        self.db.commit()
        return cur.lastrowid

    def finish_stage(self, run_id, state, result=None, error=None):
        if state not in {"done", "failed"}:
            raise ValueError("terminal stage state must be done or failed")
        now = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
        self.db.execute(
            "UPDATE semantic_stage_runs SET state=?,finished_at=?,result_json=?,error=? WHERE id=?",
            (
                state,
                now,
                json.dumps(result, ensure_ascii=False, default=str) if result is not None else None,
                error,
                run_id,
            ),
        )
        self.db.commit()

    def stage_states(self, limit=200):
        rows = self.db.execute(
            "SELECT id,stage,state,started_at,finished_at,estimate_json,result_json,error "
            "FROM semantic_stage_runs ORDER BY id DESC LIMIT ?",
            (limit + 1,),
        ).fetchall()
        has_more = len(rows) > limit
        rows = rows[:limit]
        latest, history = {}, []
        for row in rows:
            item = dict(row)
            decode_errors = []
            for name in ("estimate_json", "result_json"):
                value = item.pop(name)
                try:
                    item[name.removesuffix("_json")] = json.loads(value) if value else None
                except (TypeError, ValueError):
                    item[name.removesuffix("_json")] = None
                    decode_errors.append(name.removesuffix("_json"))
            if decode_errors:
                item["unavailable_fields"] = decode_errors
            item["last_run_at"] = item["finished_at"] or item["started_at"]
            item.pop("id", None)
            latest.setdefault(item["stage"], item)
            history.append(item)
        total = self.db.execute("SELECT COUNT(*) FROM semantic_stage_runs").fetchone()[0]
        return {
            "latest": latest,
            "history": history,
            "limit": limit,
            "total": total,
            "has_more": has_more,
        }

    def cluster_rows(self, *, offset=0, limit=50):
        columns = {row["name"] for row in self.db.execute("PRAGMA table_info(clusters)")}
        select = ",".join(
            f"c.{name} AS {name}" if name in columns else f"NULL AS {name}"
            for name in ("id", "name", "intent", "source", "coherence", "landing_url")
        )
        return self.db.execute(
            f"SELECT {select},"
            "COUNT(p.id) AS phrase_count,"
            "SUM(p.base) AS base_sum,"
            "SUM(p.exact) AS exact_sum,"
            "COUNT(p.exact) AS exact_measured "
            "FROM clusters c LEFT JOIN phrases p ON p.cluster_id=c.id "
            "AND p.status IN ('kept','review') GROUP BY c.id ORDER BY c.id LIMIT ? OFFSET ?",
            (limit, offset),
        ).fetchall()

    def cluster_row(self, cluster_id):
        columns = {row["name"] for row in self.db.execute("PRAGMA table_info(clusters)")}
        select = ",".join(
            f"c.{name} AS {name}" if name in columns else f"NULL AS {name}"
            for name in ("id", "name", "intent", "source", "coherence", "landing_url")
        )
        return self.db.execute(
            f"SELECT {select},COUNT(p.id) AS phrase_count,SUM(p.base) AS base_sum,"
            "SUM(p.exact) AS exact_sum,COUNT(p.exact) AS exact_measured "
            "FROM clusters c LEFT JOIN phrases p ON p.cluster_id=c.id "
            "AND p.status IN ('kept','review') WHERE c.id=? GROUP BY c.id",
            (cluster_id,),
        ).fetchone()

    def refresh_domains(self, own_domain=None):
        """Rebuild the ``domains`` competitor table from collected SERP rows."""
        try:
            from .stages.cluster import is_hub
        except Exception:

            def is_hub(_):
                return False

        self.db.execute("DELETE FROM domains")
        rows = self.db.execute(
            "SELECT domain, COUNT(DISTINCT phrase_norm) n, AVG(pos) ap, MIN(pos) bp "
            "FROM serp WHERE domain IS NOT NULL AND domain!='' GROUP BY domain"
        ).fetchall()
        for r in rows:
            dom = r["domain"]
            own = (own_domain or "").lower().removeprefix("www.")
            kind = (
                "own"
                if (own and (dom == own or dom.endswith("." + own)))
                else ("hub" if is_hub(dom) else "competitor")
            )
            self.db.execute(
                "INSERT OR REPLACE INTO domains(domain,n_queries,avg_pos,best_pos,kind) "
                "VALUES(?,?,?,?,?)",
                (dom, r["n"], round(r["ap"] or 0, 2), r["bp"], kind),
            )
        self.db.commit()
        return len(rows)

    # ---------- expansion journal: never expand a seed twice ----------
    def is_expanded(self, seed_norm):
        return (
            self.db.execute("SELECT 1 FROM expansions WHERE seed_norm=?", (seed_norm,)).fetchone()
            is not None
        )

    def mark_expanded(self, seed_norm, results_n=0, assoc_n=0, region="225"):
        self.db.execute(
            "INSERT OR REPLACE INTO expansions(seed_norm,expanded_at,results_n,assoc_n,region)"
            " VALUES(?,?,?,?,?)",
            (seed_norm, _now(), results_n, assoc_n, region),
        )

    # ---------- stage queries ----------
    def phrases(self, status=None, needs_exact=False, limit=None):
        q = "SELECT * FROM phrases"
        cond, args = [], []
        if status:
            cond.append("status=?")
            args.append(status)
        if needs_exact:
            cond.append("exact IS NULL")
        if cond:
            q += " WHERE " + " AND ".join(cond)
        q += " ORDER BY base DESC"
        if limit:
            q += f" LIMIT {int(limit)}"
        return self.db.execute(q, args).fetchall()

    def reexpand_candidates(self, floor):
        """kept/new phrases with base >= floor that were not expanded yet."""
        return [
            r["norm"]
            for r in self.db.execute(
                "SELECT norm FROM phrases WHERE base>=? AND status IN ('new','kept') "
                "AND norm NOT IN (SELECT seed_norm FROM expansions) ORDER BY base DESC",
                (floor,),
            )
        ]

    def serp_for(self, phrase_norm):
        return self.db.execute(
            "SELECT url,domain,pos FROM serp WHERE phrase_norm=? ORDER BY pos", (phrase_norm,)
        ).fetchall()

    def add_serp(self, phrase_norm, docs, *, fetched_at=_DEFAULT_FETCH_TIME):
        self.db.execute("DELETE FROM serp WHERE phrase_norm=?", (phrase_norm,))
        if fetched_at is _DEFAULT_FETCH_TIME:
            fetched_at = _now()
        for d in docs:
            self.db.execute(
                "INSERT INTO serp(phrase_norm,pos,url,domain,title,fetched_at) VALUES(?,?,?,?,?,?)",
                (
                    phrase_norm,
                    d.get("pos"),
                    d.get("url"),
                    d.get("domain"),
                    d.get("title"),
                    fetched_at,
                ),
            )

    # ---------- provider units ----------
    def add_run(self, stage, n_requests=0, cost_arsenkin=0, note=""):
        """Record one stage run: Yandex requests and Arsenkin limits, never a money estimate."""
        self.db.execute(
            "INSERT INTO runs(ts,stage,n_requests,cost_rub,cost_arsenkin,note) VALUES(?,?,?,?,?,?)",
            (_now(), stage, n_requests, None, cost_arsenkin, note),
        )
        self.db.commit()

    def spend(self):
        """Provider units this project consumed; money lives in the shared spend journal."""
        r = self.db.execute(
            "SELECT COALESCE(SUM(CASE WHEN stage IN ('collect','synonyms','cluster') "
            "THEN n_requests END),0) req, COALESCE(SUM(cost_arsenkin),0) lim FROM runs"
        ).fetchone()
        return {"yandex_requests": r["req"], "arsenkin": r["lim"]}

    # ---------- status ----------
    def counts(self):
        out = {}
        for r in self.db.execute("SELECT status, COUNT(*) n FROM phrases GROUP BY status"):
            out[r["status"]] = r["n"]
        out["_total"] = self.db.execute("SELECT COUNT(*) n FROM phrases").fetchone()["n"]
        out["_exact"] = self.db.execute(
            "SELECT COUNT(*) n FROM phrases WHERE exact IS NOT NULL"
        ).fetchone()["n"]
        out["_expanded"] = self.db.execute("SELECT COUNT(*) n FROM expansions").fetchone()["n"]
        out["_clusters"] = self.db.execute("SELECT COUNT(*) n FROM clusters").fetchone()["n"]
        return out

    def commit(self):
        self.db.commit()

    # ---------- export ----------
    def export(self, out_dir, *, scope="all", dry_run=False):
        out = Path(out_dir)
        clause, params = self.scope_clause(scope, "phrases")
        rows = self.db.execute(
            "SELECT phrases.* FROM phrases " + clause + " ORDER BY base DESC, id", params
        ).fetchall()
        files = [str(out / "pool.csv"), str(out / "pool.json")]
        if dry_run:
            return {
                "contract": "seohead.semantics.operation.v1",
                "mode": "preview",
                "state": "complete",
                "scope": scope,
                "rows": len(rows),
                "planned_files": files,
            }
        out.mkdir(parents=True, exist_ok=True)
        from seohead.core.tabular import neutralize_formula

        with open(out / "pool.csv", "w", newline="", encoding="utf-8-sig") as f:
            w = csv.writer(f, delimiter=";")
            columns = [r[1] for r in self.db.execute("PRAGMA table_info(phrases)")]
            w.writerow(columns)
            w.writerows([neutralize_formula(row[column]) for column in columns] for row in rows)
        with open(out / "pool.json", "w", encoding="utf-8") as f:
            json.dump([dict(r) for r in rows], f, ensure_ascii=False, indent=1)
        return len(rows)

    def scope_clause(self, scope="all", table="phrases"):
        if scope == "all":
            return "", []
        if scope == "no-landing":
            cluster_columns = {
                row["name"] for row in self.db.execute("PRAGMA table_info(clusters)")
            }
            if "landing_url" not in cluster_columns:
                raise ValueError("no-landing scope is unavailable in this legacy database")
            return (
                f"WHERE ({table}.cluster_id IN (SELECT id FROM clusters WHERE landing_url IS NULL OR landing_url='') "
                f"OR {table}.cluster_id IS NULL)",
                [],
            )
        if isinstance(scope, str) and scope.startswith("cluster:") and scope[8:].strip():
            return (
                f"WHERE {table}.cluster_id IN (SELECT id FROM clusters WHERE name=?)",
                [scope[8:].strip()],
            )
        raise ValueError("scope must be all, no-landing, or cluster:<name>")

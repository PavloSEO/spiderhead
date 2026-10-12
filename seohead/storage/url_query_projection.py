"""Optional scan.v2 URL-query projection, built only by a finished writer."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import time
from typing import Any

FORMAT = "scan_url_query_projection.v2"
_META = "scan_url_query_projection_meta"
_PAGES = "scan_url_query_projection_pages"
_BUILD_BUDGET_SECONDS = 8.0
_RESOURCE_KINDS = (
    "image",
    "media",
    "document",
    "stylesheet",
    "module",
    "manifest",
    "script",
    "css_import",
    "css_url",
    "font",
)
_PAGE_COLUMNS = (
    "url_id",
    "document_id",
    "resource_coverage_state",
    "resource_count",
    "resource_images",
    "resource_scripts",
    "resource_stylesheets",
    *(f"kind_{kind}" for kind in _RESOURCE_KINDS),
    "title_duplicate",
)
_PAGE_INDEXES = {
    "scan_url_query_resource_images",
    "scan_url_query_resource_scripts",
    "scan_url_query_resource_stylesheets",
    "scan_url_query_title_duplicate",
    *(f"scan_url_query_kind_{kind}" for kind in _RESOURCE_KINDS),
}


def load(con: sqlite3.Connection) -> dict[str, Any] | None:
    """Return a source-bound projection; never create or migrate anything on a read."""
    if con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return None
    tables = {
        row[0]
        for row in con.execute(
            "SELECT name FROM main.sqlite_master WHERE type='table' AND name IN (?,?)",
            (_META, _PAGES),
        )
    }
    if tables != {_META, _PAGES}:
        return None
    try:
        source = con.execute(
            "SELECT scan_uuid,evidence_revision,capabilities_json,start_url,lifecycle "
            "FROM scan WHERE singleton=1"
        ).fetchone()
        row = con.execute(f"SELECT * FROM main.{_META} WHERE singleton=1").fetchone()
        if source is None or row is None or source["lifecycle"] != "finished":
            return None
        page_columns = tuple(item[1] for item in con.execute(f"PRAGMA main.table_info({_PAGES})"))
        page_indexes = {
            item[0]
            for item in con.execute(
                "SELECT name FROM main.sqlite_master WHERE type='index' AND name GLOB 'scan_url_query_*'"
            )
        }
        if page_columns != _PAGE_COLUMNS or not page_indexes >= _PAGE_INDEXES:
            return None
        capabilities_sha256 = hashlib.sha256(source["capabilities_json"].encode()).hexdigest()
        if (
            row["projection_version"] != FORMAT
            or row["scan_uuid"] != source["scan_uuid"]
            or row["evidence_revision"] != source["evidence_revision"]
            or row["capabilities_sha256"] != capabilities_sha256
            or row["start_url"] != source["start_url"]
        ):
            return None
        counts = json.loads(row["facet_counts_json"])
        states = json.loads(row["facet_states_json"])
        coverage_counts = json.loads(row["resource_coverage_counts_json"])
        page_count = row["page_count"]
        projection_rows = con.execute(f"SELECT COUNT(*) FROM main.{_PAGES}").fetchone()[0]
        if (
            not isinstance(counts, dict)
            or not isinstance(states, dict)
            or not isinstance(coverage_counts, dict)
        ):
            return None
        from .url_query import FACET_GROUPS

        if (
            type(page_count) is not int
            or page_count < 0
            or projection_rows != page_count
            or set(counts) != set(states)
            or set(counts) != set(FACET_GROUPS)
        ):
            return None
        for name, state in states.items():
            if state == "exact":
                if type(counts[name]) is not int or not 0 <= counts[name] <= page_count:
                    return None
            elif state in {"unavailable", "capped"}:
                if counts[name] is not None:
                    return None
            else:
                return None
        if set(coverage_counts) - {"complete", "partial", "unavailable"} or any(
            type(value) is not int or value < 0 for value in coverage_counts.values()
        ):
            return None
        if sum(coverage_counts.values()) != page_count:
            return None
        if row["resource_coverage_state"] not in {"complete", "unavailable"}:
            return None
        if row["resource_coverage_state"] == "complete" and (
            coverage_counts.get("complete", 0) != page_count
            or coverage_counts.get("partial", 0)
            or coverage_counts.get("unavailable", 0)
        ):
            return None
        return {
            "page_count": page_count,
            "resource_coverage_state": row["resource_coverage_state"],
            "resource_coverage_counts": coverage_counts,
            "facets": counts,
            "facet_states": states,
        }
    except (sqlite3.Error, TypeError, ValueError, KeyError, IndexError):
        return None


def invalidate(con: sqlite3.Connection) -> None:
    """Remove the published source binding inside a writer's current transaction."""
    if con.execute(
        "SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?", (_META,)
    ).fetchone():
        con.execute(f"DELETE FROM main.{_META}")


def persist(con: sqlite3.Connection, *, timeout_seconds: float = _BUILD_BUDGET_SECONDS) -> bool:
    """Build an optional v2 page/facet projection without making finalization depend on it."""
    if con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return False
    if type(timeout_seconds) not in {int, float} or not 0.1 <= timeout_seconds <= 60:
        raise ValueError("query projection timeout must be 0.1..60 seconds")
    if not con.in_transaction:
        return False
    prior_query_only = con.execute("PRAGMA query_only").fetchone()[0]
    deadline = time.monotonic() + timeout_seconds
    savepoint = False
    try:
        con.execute("PRAGMA query_only=OFF")
        # A failed optional rebuild must never leave the previous publication usable.
        invalidate(con)
        con.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
        con.execute("SAVEPOINT build_scan_url_query_projection")
        savepoint = True
        from .url_query import FACET_GROUPS, _Budget, _facet_counts

        source = con.execute(
            "SELECT scan_uuid,evidence_revision,capabilities_json,start_url,lifecycle "
            "FROM scan WHERE singleton=1"
        ).fetchone()
        if source is None or source["lifecycle"] != "finished":
            raise ValueError("URL query projection requires a finished scan.v2")
        capabilities_sha256 = hashlib.sha256(source["capabilities_json"].encode()).hexdigest()
        resource_tables = {
            row[0]
            for row in con.execute(
                "SELECT name FROM main.sqlite_master WHERE type='table' AND name IN "
                "('resource_graph_occurrences','resource_graph_fetches')"
            )
        }
        resource_tables_complete = resource_tables == {
            "resource_graph_occurrences",
            "resource_graph_fetches",
        }

        con.execute("DROP TABLE IF EXISTS temp.query_projection_duplicate_titles")
        con.execute(
            "CREATE TEMP TABLE query_projection_duplicate_titles(title TEXT PRIMARY KEY) WITHOUT ROWID"
        )
        con.execute(
            "INSERT INTO temp.query_projection_duplicate_titles(title) "
            "SELECT lower(title) FROM pages WHERE trim(title)<>'' "
            "GROUP BY lower(title) HAVING COUNT(*)>1"
        )

        kind_columns = ",".join(f"kind_{kind} INTEGER" for kind in _RESOURCE_KINDS)
        kind_values = ",".join(f"SUM(kind='{kind}')" for kind in _RESOURCE_KINDS)
        con.execute("DROP TABLE IF EXISTS temp.query_projection_resource_counts")
        con.execute(
            "CREATE TEMP TABLE query_projection_resource_counts("
            "page_url_id INTEGER NOT NULL,source_document_id INTEGER NOT NULL,"
            "resource_count INTEGER NOT NULL,resource_images INTEGER NOT NULL,"
            "resource_scripts INTEGER NOT NULL,resource_stylesheets INTEGER NOT NULL,"
            f"{kind_columns},"
            "PRIMARY KEY(page_url_id,source_document_id)) WITHOUT ROWID"
        )
        if resource_tables_complete:
            con.execute(
                "INSERT INTO temp.query_projection_resource_counts "
                "SELECT page_url_id,source_document_id,COUNT(*),"
                "SUM(kind='image'),SUM(kind IN ('script','module')),"
                "SUM(kind='stylesheet')," + kind_values + " FROM resource_graph_occurrences "
                "GROUP BY page_url_id,source_document_id"
            )

        con.execute("DROP TABLE IF EXISTS temp.query_projection_pages")
        kind_page_columns = ",".join(f"kind_{kind} INTEGER" for kind in _RESOURCE_KINDS)
        con.execute(
            "CREATE TEMP TABLE query_projection_pages("
            "url_id INTEGER PRIMARY KEY,document_id INTEGER,resource_coverage_state TEXT NOT NULL,"
            "resource_count INTEGER,resource_images INTEGER,resource_scripts INTEGER,"
            "resource_stylesheets INTEGER," + kind_page_columns + ","
            "title_duplicate INTEGER NOT NULL) WITHOUT ROWID"
        )
        con.execute(
            "INSERT INTO temp.query_projection_pages "
            "SELECT p.url_id,p.document_id,"
            "CASE WHEN NOT ? OR p.document_id IS NULL THEN 'unavailable' "
            "WHEN c.completeness='complete' THEN 'complete' "
            "WHEN c.completeness='partial' THEN 'partial' ELSE 'unavailable' END,"
            "CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_count,0) END,"
            "CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_images,0) END,"
            "CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_scripts,0) END,"
            "CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_stylesheets,0) END,"
            + ",".join(
                f"CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.kind_{kind},0) END"
                for kind in _RESOURCE_KINDS
            )
            + ","
            "CASE WHEN d.title IS NULL THEN 0 ELSE 1 END "
            "FROM pages p LEFT JOIN context_items c ON c.kind='resource_graph_coverage' "
            "AND c.item_key='document:'||p.document_id "
            "LEFT JOIN temp.query_projection_resource_counts r "
            "ON r.page_url_id=p.url_id AND r.source_document_id=p.document_id "
            "LEFT JOIN temp.query_projection_duplicate_titles d "
            "ON d.title=lower(p.title) AND trim(p.title)<>''",
            (int(resource_tables_complete),) * (5 + len(_RESOURCE_KINDS)),
        )
        page_count = con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
        coverage_counts = {
            row[0]: row[1]
            for row in con.execute(
                "SELECT resource_coverage_state,COUNT(*) FROM temp.query_projection_pages "
                "GROUP BY resource_coverage_state"
            )
        }
        resource_state = (
            "complete"
            if resource_tables_complete and coverage_counts.get("complete", 0) == page_count
            else "unavailable"
        )

        projection_facets = {
            **FACET_GROUPS,
            "title_duplicate": "q.title_duplicate=1",
            "with_resources": "q.resource_count>0",
            "with_images": "q.resource_images>0",
            "with_scripts": "q.resource_scripts>0",
            "with_stylesheets": "q.resource_stylesheets>0",
        }
        facet_counts, facet_states = _facet_counts(
            con,
            _Budget(con),
            list(FACET_GROUPS),
            " JOIN urls u ON u.url_id=p.url_id "
            "LEFT JOIN temp.query_projection_pages q ON q.url_id=p.url_id",
            "",
            [],
            max(0.1, min(timeout_seconds, deadline - time.monotonic())),
            resource_state,
            projection_facets,
        )
        con.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)

        existing_page_columns = [row[1] for row in con.execute(f"PRAGMA main.table_info({_PAGES})")]
        if existing_page_columns and tuple(existing_page_columns) != _PAGE_COLUMNS:
            con.execute(f"DROP TABLE IF EXISTS main.{_META}")
            con.execute(f"DROP TABLE IF EXISTS main.{_PAGES}")
        con.execute(
            f"CREATE TABLE IF NOT EXISTS main.{_PAGES}("
            "url_id INTEGER PRIMARY KEY,document_id INTEGER,resource_coverage_state TEXT NOT NULL,"
            "resource_count INTEGER,resource_images INTEGER,resource_scripts INTEGER,"
            "resource_stylesheets INTEGER," + kind_columns + ",title_duplicate INTEGER NOT NULL)"
        )
        con.execute(
            f"CREATE TABLE IF NOT EXISTS main.{_META}("
            "singleton INTEGER PRIMARY KEY CHECK(singleton=1),projection_version TEXT NOT NULL,"
            "scan_uuid TEXT NOT NULL,evidence_revision INTEGER NOT NULL,"
            "capabilities_sha256 TEXT NOT NULL,start_url TEXT,page_count INTEGER NOT NULL,"
            "resource_coverage_state TEXT NOT NULL,resource_coverage_counts_json TEXT NOT NULL,"
            "facet_counts_json TEXT NOT NULL,facet_states_json TEXT NOT NULL)"
        )
        con.execute(f"DELETE FROM main.{_PAGES}")
        con.execute(f"INSERT INTO main.{_PAGES} SELECT * FROM temp.query_projection_pages")
        con.execute(
            f"CREATE INDEX IF NOT EXISTS scan_url_query_resource_images ON {_PAGES}(resource_images,url_id)"
        )
        con.execute(
            f"CREATE INDEX IF NOT EXISTS scan_url_query_resource_scripts ON {_PAGES}(resource_scripts,url_id)"
        )
        con.execute(
            f"CREATE INDEX IF NOT EXISTS scan_url_query_resource_stylesheets ON {_PAGES}(resource_stylesheets,url_id)"
        )
        for kind in _RESOURCE_KINDS:
            con.execute(
                f"CREATE INDEX IF NOT EXISTS scan_url_query_kind_{kind} ON {_PAGES}(kind_{kind},url_id)"
            )
        con.execute(
            f"CREATE INDEX IF NOT EXISTS scan_url_query_title_duplicate ON {_PAGES}(title_duplicate,url_id)"
        )
        con.execute(f"DELETE FROM main.{_META}")
        con.execute(
            f"INSERT INTO main.{_META} VALUES(1,?,?,?,?,?,?,?,?,?,?)",
            (
                FORMAT,
                source["scan_uuid"],
                source["evidence_revision"],
                capabilities_sha256,
                source["start_url"],
                page_count,
                resource_state,
                json.dumps(coverage_counts, sort_keys=True, separators=(",", ":")),
                json.dumps(facet_counts, sort_keys=True, separators=(",", ":")),
                json.dumps(facet_states, sort_keys=True, separators=(",", ":")),
            ),
        )
        con.execute("RELEASE SAVEPOINT build_scan_url_query_projection")
        return True
    except Exception:
        con.set_progress_handler(None, 0)
        if savepoint:
            try:
                con.execute("ROLLBACK TO SAVEPOINT build_scan_url_query_projection")
                con.execute("RELEASE SAVEPOINT build_scan_url_query_projection")
            except sqlite3.Error:
                pass
        return False
    finally:
        con.set_progress_handler(None, 0)
        con.execute(f"PRAGMA query_only={'ON' if prior_query_only else 'OFF'}")

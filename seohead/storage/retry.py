"""Explicit scan.v2 retry/requeue storage; reading a scan never upgrades it."""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import sqlite3
import uuid
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from seohead.core.sqlite import open_readonly

from ..core.filesystem import file_sha256
from . import APPLICATION_ID, ScanError, open_scan
from .history import _hold_writer_lock, _regular
from .native_scan import NativeScan, _utc

V2_USER_VERSION = 2
V2_FORMAT = "scan.v2"
_WHERE_FIELDS = {
    "url": "u.url",
    "status_code": "p.status_code",
    "content_type": "p.content_type",
    "crawl_depth": "p.crawl_depth",
    "representation": "p.representation",
    "page_ordinal": "p.page_ordinal",
}
_WHERE = re.compile(r"\s*([a-z_]+)\s*(=|!=|<=|>=|<|>)\s*(.+?)\s*\Z")


def _predicate(where: str) -> tuple[str, list[Any], list[dict[str, Any]]]:
    """Compile a small, value-bound predicate instead of accepting SQL text."""
    if not isinstance(where, str) or not where or len(where) > 2048:
        raise ValueError("where must be a nonempty predicate under 2048 characters")
    pieces = re.split(r"\s+AND\s+", where, flags=re.IGNORECASE)
    if not 1 <= len(pieces) <= 16:
        raise ValueError("where accepts one to sixteen AND-connected conditions")
    clauses, values, recorded = [], [], []
    for piece in pieces:
        match = _WHERE.fullmatch(piece)
        if match is None or match.group(1) not in _WHERE_FIELDS:
            raise ValueError("where uses an unsupported field or operator")
        field, operator, raw = match.groups()
        try:
            value = json.loads(raw)
        except ValueError:
            value = int(raw) if raw.isdecimal() else None
        if type(value) not in {str, int}:
            raise ValueError("where values must be JSON strings or integers")
        if field in {"status_code", "crawl_depth", "page_ordinal"} and type(value) is not int:
            raise ValueError(f"where {field} requires an integer value")
        if field in {"url", "content_type", "representation"} and type(value) is not str:
            raise ValueError(f"where {field} requires a JSON string value")
        clauses.append(f"{_WHERE_FIELDS[field]} {operator} ?")
        values.append(value)
        recorded.append({"field": field, "operator": operator, "value": value})
    return " AND ".join(clauses), values, recorded


def _v2_tables(con: sqlite3.Connection) -> None:
    names = {
        row[0]
        for row in con.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('retry_attempts','retry_transitions')"
        )
    }
    if names != {"retry_attempts", "retry_transitions"}:
        raise ScanError("scan.v2 retry history tables are missing")


def validate_v2(con: sqlite3.Connection, *, require_audit: bool = False) -> None:
    """Validate the v2 active frontier while preserving v1 backup provenance."""
    if con.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID:
        raise ScanError("foreign application_id")
    if con.execute("PRAGMA user_version").fetchone()[0] != V2_USER_VERSION:
        raise ScanError("unsupported scan.v2 user version")
    _v2_tables(con)
    scan = con.execute("SELECT * FROM scan WHERE singleton=1").fetchone()
    if (
        scan is None
        or scan["format_version"] != V2_FORMAT
        or scan["source_kind"] not in {"native", "reanalysis"}
    ):
        raise ScanError("scan.v2 format header is invalid")
    if con.execute("SELECT COUNT(*) FROM scan").fetchone()[0] != 1:
        raise ScanError("scan.v2 requires one scan header")
    if con.execute(
        "SELECT 1 FROM frontier f LEFT JOIN pages p USING(url_id) "
        "WHERE (f.state='done') != (p.url_id IS NOT NULL) LIMIT 1"
    ).fetchone():
        raise ScanError("scan.v2 active frontier and page population disagree")
    from .resource_graph import validate as validate_resource_graph

    validate_resource_graph(con)
    validate_discovery_ledger(con)
    if require_audit and con.execute("SELECT 1 FROM audit WHERE singleton=1").fetchone() is None:
        raise ScanError("scan.v2 has no current audit")
    for row in con.execute("SELECT * FROM retry_attempts"):
        if row["operation"] not in {"requeue", "import_urls"} or not re.fullmatch(
            r"[0-9a-f]{64}", row["backup_sha256"]
        ):
            raise ScanError("scan.v2 retry attempt is invalid")
        json.loads(row["where_json"])
    for row in con.execute("SELECT * FROM retry_transitions"):
        if (
            row["from_frontier_state"] not in {"done", "external"}
            or row["to_frontier_state"] != "queued"
        ):
            raise ScanError("scan.v2 retry transition is invalid")
        if row["prior_page_json"] is not None:
            json.loads(row["prior_page_json"])


def validate_discovery_ledger(con: sqlite3.Connection) -> None:
    """Validate optional v2 discovery evidence without mutating a reader connection."""
    names = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    present = {"discovery_occurrences", "discovery_ledger_coverage"} & names
    if not present:
        return
    if present != {"discovery_occurrences", "discovery_ledger_coverage"}:
        raise ScanError("discovery ledger schema is incomplete")
    relations = {
        "seed",
        "hyperlink",
        "redirect",
        "canonical",
        "alternate",
        "hreflang",
        "x_default",
        "next",
        "prev",
        "refresh",
        "form_action",
        "http_link",
    }
    outcomes = {"queued", "fetched", "excluded", "blocked", "unresolved", "unmeasured"}
    for row in con.execute("SELECT * FROM discovery_occurrences"):
        if (
            not isinstance(row["occurrence_key"], str)
            or not row["occurrence_key"]
            or row["relation"] not in relations
            or row["outcome"] not in outcomes
            or row["representation"] not in {"static", "rendered", "legacy_fragment", "unmeasured"}
            or not isinstance(row["raw_value"], str)
            or not isinstance(row["resolved_value"], str)
            or not isinstance(row["reason"], str)
            or (row["depth"] is not None and (type(row["depth"]) is not int or row["depth"] < 0))
            or not isinstance(json.loads(row["attributes_json"]), dict)
        ):
            raise ScanError("discovery occurrence is invalid")
        if (
            row["source_url_id"] is not None
            and not con.execute(
                "SELECT 1 FROM urls WHERE url_id=?", (row["source_url_id"],)
            ).fetchone()
        ):
            raise ScanError("discovery occurrence source URL is missing")
        if (
            row["source_document_id"] is not None
            and not con.execute(
                "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
                (row["source_document_id"], row["source_url_id"], row["representation"]),
            ).fetchone()
        ):
            raise ScanError("discovery occurrence source document is invalid")
        if (
            row["source_response_id"] is not None
            and not con.execute(
                "SELECT 1 FROM documents WHERE document_id=? AND source_response_id=?",
                (row["source_document_id"], row["source_response_id"]),
            ).fetchone()
        ):
            raise ScanError("discovery occurrence source response is invalid")
        target = con.execute(
            "SELECT url_id FROM urls WHERE url=?", (row["resolved_value"],)
        ).fetchone()
        recorded_target = row["target_url_id"]
        if recorded_target is None:
            # This is the capture-time lookup, not the current catalogue. A later
            # sitemap/seed may retain a target that was unresolved or excluded
            # when the immutable occurrence was written. Claimed queued/fetched
            # targets still require a recorded identity, as do unexplained gaps.
            if (
                row["outcome"] not in {"unresolved", "unmeasured", "excluded"}
                or not row["reason"].strip()
            ):
                raise ScanError(
                    "discovery occurrence has no explained capture-time target identity"
                )
        elif target is None or target[0] != recorded_target:
            raise ScanError(
                "discovery occurrence target identity disagrees with its resolved value"
            )
    for row in con.execute("SELECT * FROM discovery_ledger_coverage"):
        if (
            row["representation"] not in {"static", "rendered", "legacy_fragment"}
            or type(row["captured"]) is not int
            or type(row["omitted"]) is not int
            or row["captured"] < 0
            or row["omitted"] < 0
            or row["captured"] > 2_000
            or row["state"] not in {"complete", "partial"}
            or not isinstance(row["reason"], str)
            or (row["state"] == "complete" and (row["omitted"] != 0 or row["reason"]))
            or (row["state"] == "partial" and (row["omitted"] < 1 or not row["reason"]))
            or not con.execute(
                "SELECT 1 FROM documents WHERE document_id=? AND representation=?",
                (row["source_document_id"], row["representation"]),
            ).fetchone()
        ):
            raise ScanError("discovery ledger coverage is invalid")
        count = con.execute(
            "SELECT COUNT(*) FROM discovery_occurrences WHERE source_document_id=? AND representation=?",
            (row["source_document_id"], row["representation"]),
        ).fetchone()[0]
        if count != row["captured"]:
            raise ScanError("discovery ledger coverage count disagrees with occurrences")


def _validate_copy(path: Path) -> None:
    with contextlib.closing(open_scan(path, require_audit=False)):
        return


def _backup(path: Path, backup_path: Path) -> str:
    if not isinstance(backup_path, Path) or os.path.lexists(backup_path):
        raise ScanError("retry requires a new backup_path")
    con = open_readonly(path.absolute(), row_factory=sqlite3.Row)
    try:
        reader = SimpleNamespace(
            path=path, con=con, inspect=lambda copy: _validate_copy(Path(copy))
        )
        NativeScan.snapshot(reader, backup_path)
    finally:
        con.close()
    _validate_copy(backup_path)
    return file_sha256(backup_path)


def upgrade_to_v2(con: sqlite3.Connection) -> None:
    version = con.execute("PRAGMA user_version").fetchone()[0]
    if version == V2_USER_VERSION:
        validate_v2(con, require_audit=False)
        return
    if version != 1:
        raise ScanError("retry supports scan.v1 or scan.v2 only")
    row = con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()
    if row is None or row[0] != "scan.v1":
        raise ScanError("explicit retry upgrade requires a scan.v1 header")
    con.execute("ALTER TABLE scan RENAME TO scan_v1_upgrade")
    con.execute(
        "CREATE TABLE scan (singleton INTEGER PRIMARY KEY CHECK (singleton = 1),"
        "scan_uuid TEXT NOT NULL UNIQUE,format_version TEXT NOT NULL CHECK (format_version = 'scan.v2'),"
        "evidence_version TEXT NOT NULL,writer_version TEXT NOT NULL,writer_revision TEXT NOT NULL,"
        "runtime_versions_json TEXT NOT NULL,created_at TEXT NOT NULL,finished_at TEXT,"
        "source_kind TEXT NOT NULL CHECK (source_kind IN ('native','legacy_import','reanalysis')),"
        "parent_scan_uuid TEXT,start_url TEXT,config_json TEXT NOT NULL,config_fingerprint TEXT NOT NULL,"
        "lifecycle TEXT NOT NULL CHECK (lifecycle IN ('running','interrupted','finished','failed')),"
        "finish_reason TEXT NOT NULL,crawl_partial INTEGER NOT NULL CHECK (crawl_partial IN (0,1)),"
        "corpus_partial INTEGER NOT NULL CHECK (corpus_partial IN (0,1)),"
        "evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 0),limitations_json TEXT NOT NULL,"
        "capabilities_json TEXT NOT NULL,retention_json TEXT NOT NULL,pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)))"
    )
    con.execute(
        "INSERT INTO scan SELECT singleton,scan_uuid,'scan.v2',evidence_version,writer_version,"
        "writer_revision,runtime_versions_json,created_at,finished_at,source_kind,parent_scan_uuid,"
        "start_url,config_json,config_fingerprint,lifecycle,finish_reason,crawl_partial,corpus_partial,"
        "evidence_revision,limitations_json,capabilities_json,retention_json,pinned FROM scan_v1_upgrade"
    )
    con.execute("DROP TABLE scan_v1_upgrade")
    con.execute(
        "CREATE TABLE retry_attempts (attempt_id INTEGER PRIMARY KEY,attempt_uuid TEXT NOT NULL UNIQUE,"
        "operation TEXT NOT NULL CHECK (operation IN ('requeue','import_urls')),requested_at TEXT NOT NULL,"
        "where_json TEXT NOT NULL,backup_path TEXT NOT NULL,backup_sha256 TEXT NOT NULL "
        "CHECK (length(backup_sha256) = 64),source_scan_uuid TEXT)"
    )
    con.execute(
        "CREATE TABLE retry_transitions (attempt_id INTEGER NOT NULL REFERENCES retry_attempts(attempt_id),"
        "url_id INTEGER NOT NULL,url TEXT NOT NULL,from_frontier_state TEXT NOT NULL,"
        "to_frontier_state TEXT NOT NULL,prior_page_json TEXT,prior_document_id INTEGER,"
        "prior_evidence_sha256 TEXT,removed_links INTEGER NOT NULL CHECK (removed_links >= 0),"
        "removed_forms INTEGER NOT NULL CHECK (removed_forms >= 0),removed_resource_refs INTEGER NOT NULL "
        "CHECK (removed_resource_refs >= 0),removed_contexts INTEGER NOT NULL CHECK (removed_contexts >= 0),"
        "PRIMARY KEY (attempt_id,url_id))"
    )
    con.execute("CREATE INDEX retry_transitions_url_id ON retry_transitions(url_id,attempt_id)")
    con.execute(f"PRAGMA user_version={V2_USER_VERSION}")
    from .discovery_ledger import ensure_schema as ensure_discovery_ledger

    ensure_discovery_ledger(con)
    from .external_checks import ensure_schema as ensure_external_checks

    ensure_external_checks(con)


def _writer(path: Path) -> sqlite3.Connection:
    con = sqlite3.connect(path.absolute().as_uri() + "?mode=rw", uri=True, timeout=5)
    con.row_factory = sqlite3.Row
    con.execute("PRAGMA foreign_keys=ON")
    con.execute("PRAGMA trusted_schema=OFF")
    con.execute("PRAGMA synchronous=FULL")
    return con


def _attempt(
    con: sqlite3.Connection,
    *,
    operation: str,
    selection: list[dict[str, Any]],
    backup_path: Path,
    backup_sha256: str,
    source_scan_uuid: str | None,
) -> int:
    con.execute(
        "INSERT INTO retry_attempts(attempt_uuid,operation,requested_at,where_json,backup_path,backup_sha256,source_scan_uuid) "
        "VALUES(?,?,?,?,?,?,?)",
        (
            str(uuid.uuid4()),
            operation,
            _utc(),
            json.dumps(selection, sort_keys=True, separators=(",", ":")),
            str(backup_path),
            backup_sha256,
            source_scan_uuid,
        ),
    )
    return int(con.execute("SELECT last_insert_rowid()").fetchone()[0])


def _evidence_digest(
    con: sqlite3.Connection, url_id: int, queue_ordinal: int
) -> tuple[str, dict[str, int]]:
    rows = {
        "links": con.execute(
            "SELECT COUNT(*) FROM links WHERE source_url_id=?", (url_id,)
        ).fetchone()[0],
        "forms": con.execute(
            "SELECT COUNT(*) FROM forms WHERE page_url_id=?", (url_id,)
        ).fetchone()[0],
        "resource_refs": con.execute(
            "SELECT COUNT(*) FROM resource_refs WHERE page_url_id=?", (url_id,)
        ).fetchone()[0],
        "contexts": con.execute(
            "SELECT COUNT(*) FROM context_items WHERE item_key LIKE ? OR (kind='native_commit' AND item_key=?)",
            (f"page:{url_id}:%", str(queue_ordinal)),
        ).fetchone()[0],
    }
    encoded = json.dumps(rows, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest(), rows


def requeue_scan(
    input_path: str | Path,
    *,
    where: str,
    backup_path: str | Path,
    from_scan: str | Path | None = None,
) -> dict[str, Any]:
    """Upgrade explicitly and requeue selected done pages with durable transitions."""
    predicate, values, selection = _predicate(where)
    path, backup = Path(input_path).absolute(), Path(backup_path).absolute()
    _regular(path)
    source_urls: list[str] | None = None
    source_scan_uuid: str | None = None
    if from_scan is not None:
        source = Path(from_scan).absolute()
        if source == path:
            raise ValueError("from_scan must differ from the scan being requeued")
        with contextlib.closing(open_scan(source, require_audit=False)) as source_con:
            source_scan_uuid = source_con.execute(
                "SELECT scan_uuid FROM scan WHERE singleton=1"
            ).fetchone()[0]
            source_urls = [
                row[0]
                for row in source_con.execute(
                    "SELECT u.url FROM pages p JOIN urls u USING(url_id) WHERE "
                    + predicate
                    + " ORDER BY p.page_ordinal",
                    values,
                )
            ]
        if not source_urls:
            raise ScanError("where matched no pages in from_scan")
        if len(source_urls) > 20_000:
            raise ScanError("from_scan selection exceeds the 20,000 URL retry bound")
    lock = _hold_writer_lock(path)
    con: sqlite3.Connection | None = None
    try:
        backup_sha256 = _backup(path, backup)
        con = _writer(path)
        con.execute("BEGIN IMMEDIATE")
        upgrade_to_v2(con)
        query = (
            "SELECT p.*,f.state AS frontier_state,f.queue_ordinal,u.url FROM pages p "
            "JOIN frontier f USING(url_id) JOIN urls u USING(url_id) WHERE f.state='done' AND "
        )
        query_values: list[Any] = list(values)
        if source_urls is None:
            query += predicate
        else:
            query += "u.url IN (" + ",".join("?" for _ in source_urls) + ")"
            query_values.extend(source_urls)
        rows = list(con.execute(query + " ORDER BY p.page_ordinal", query_values))
        if not rows:
            raise ScanError("where matched no completed pages to requeue")
        attempt_id = _attempt(
            con,
            operation="requeue",
            selection=selection,
            backup_path=backup,
            backup_sha256=backup_sha256,
            source_scan_uuid=source_scan_uuid,
        )
        from .url_query_projection import invalidate as invalidate_url_query_projection

        invalidate_url_query_projection(con)
        from .resource_graph import invalidate_pages as invalidate_resource_graph_pages

        invalidate_resource_graph_pages(con, [row["url_id"] for row in rows])
        for row in rows:
            page = dict(row)
            digest, counts = _evidence_digest(con, row["url_id"], row["queue_ordinal"])
            con.execute(
                "INSERT INTO retry_transitions VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    attempt_id,
                    row["url_id"],
                    row["url"],
                    "done",
                    "queued",
                    json.dumps(page, sort_keys=True, separators=(",", ":")),
                    row["document_id"],
                    digest,
                    counts["links"],
                    counts["forms"],
                    counts["resource_refs"],
                    counts["contexts"],
                ),
            )
            con.execute("DELETE FROM links WHERE source_url_id=?", (row["url_id"],))
            con.execute("DELETE FROM forms WHERE page_url_id=?", (row["url_id"],))
            con.execute("DELETE FROM resource_refs WHERE page_url_id=?", (row["url_id"],))
            con.execute(
                "DELETE FROM context_items WHERE item_key LIKE ? OR (kind='native_commit' AND item_key=?)",
                (f"page:{row['url_id']}:%", str(row["queue_ordinal"])),
            )
            con.execute("DELETE FROM pages WHERE url_id=?", (row["url_id"],))
            con.execute(
                "UPDATE frontier SET state='queued' WHERE url_id=? AND state='done'",
                (row["url_id"],),
            )
        from .corpus import corpus_summary
        from .native_scan import _reanalysis_capability

        metadata = con.execute(
            "SELECT capabilities_json,retention_json,source_kind FROM scan WHERE singleton=1"
        ).fetchone()
        capabilities = json.loads(metadata["capabilities_json"])
        summary = corpus_summary(con, json.loads(metadata["retention_json"]))
        capabilities.update(summary["capabilities"])
        if metadata["source_kind"] == "native":
            capabilities["offline_reanalysis"] = _reanalysis_capability(con)
        evidence_revision = (
            con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
            + con.execute(
                "SELECT COUNT(*) FROM context_items WHERE kind='resource_commit'"
            ).fetchone()[0]
            + con.execute(
                "SELECT COUNT(*) FROM documents WHERE representation IN ('rendered','legacy_fragment')"
            ).fetchone()[0]
        )
        con.execute(
            "UPDATE scan SET lifecycle='running',finished_at=NULL,finish_reason='retry_requeue',"
            "crawl_partial=1,corpus_partial=?,capabilities_json=?,evidence_revision=? WHERE singleton=1",
            (
                int(summary["corpus_partial"]),
                json.dumps(capabilities, sort_keys=True, separators=(",", ":")),
                evidence_revision,
            ),
        )
        con.execute("DELETE FROM audit")
        con.commit()
        return {
            "scan": str(path),
            "backup": str(backup),
            "attempt_id": attempt_id,
            "requeued": len(rows),
        }
    except BaseException:
        if con is not None:
            con.rollback()
        raise
    finally:
        if con is not None:
            con.close()
        from seohead.core.filesystem import unlock

        unlock(lock)
        os.close(lock)


def scan_import_urls(
    input_path: str | Path,
    *,
    urls_file: str | Path,
    backup_path: str | Path,
) -> dict[str, Any]:
    """Import external URL-list input through the target scan's existing admission rules."""
    from seohead.crawl.list_input import read_url_list
    from seohead.crawl.spider import Scope, _strip_fragment

    target, backup = Path(input_path).absolute(), Path(backup_path).absolute()
    listed_urls = read_url_list(urls_file)
    if len(listed_urls) > 20_000:
        raise ScanError("external URL list exceeds the 20,000 URL import bound")
    _regular(target)
    lock = _hold_writer_lock(target)
    con: sqlite3.Connection | None = None
    try:
        backup_sha256 = _backup(target, backup)
        con = _writer(target)
        con.execute("BEGIN IMMEDIATE")
        upgrade_to_v2(con)
        scan = con.execute("SELECT start_url,config_json FROM scan WHERE singleton=1").fetchone()
        config = json.loads(scan["config_json"])
        scope = Scope.from_config(config["scope"])
        from urllib.parse import urlsplit

        start_host = (urlsplit(scan["start_url"] or "").hostname or "").lower()
        if not start_host:
            raise ScanError("scan has no start host for external URL admission")
        attempt_id = _attempt(
            con,
            operation="import_urls",
            selection=[{"urls_file": str(Path(urls_file).absolute())}],
            backup_path=backup,
            backup_sha256=backup_sha256,
            source_scan_uuid=None,
        )
        next_ordinal = con.execute(
            "SELECT COALESCE(MAX(queue_ordinal)+1,0) FROM frontier"
        ).fetchone()[0]
        added, rejected = 0, 0
        for raw_url in listed_urls:
            url = _strip_fragment(raw_url)
            reason = scope.rejection(url, start_host)
            if reason or (
                config["limits"]["max_url_length"] and len(url) > config["limits"]["max_url_length"]
            ):
                rejected += 1
                continue
            existing = con.execute("SELECT url_id FROM urls WHERE url=?", (url,)).fetchone()
            if (
                existing is not None
                and con.execute("SELECT 1 FROM frontier WHERE url_id=?", (existing[0],)).fetchone()
            ):
                continue
            parts = urlsplit(url)
            query_limit = config["limits"]["max_query_variants_per_path"]
            if parts.query:
                count = con.execute(
                    "SELECT COUNT(*) FROM query_variants WHERE path_key=?", (parts.path or "/",)
                ).fetchone()[0]
                known = con.execute(
                    "SELECT 1 FROM query_variants WHERE path_key=? AND query_key=?",
                    (parts.path or "/", parts.query),
                ).fetchone()
                if not known and query_limit and count >= query_limit:
                    rejected += 1
                    continue
                con.execute(
                    "INSERT OR IGNORE INTO query_variants(path_key,query_key) VALUES(?,?)",
                    (parts.path or "/", parts.query),
                )
            con.execute("INSERT OR IGNORE INTO urls(url) VALUES(?)", (url,))
            url_id = con.execute("SELECT url_id FROM urls WHERE url=?", (url,)).fetchone()[0]
            con.execute(
                "INSERT INTO frontier(url_id,queue_ordinal,depth,state) VALUES(?,?,?,'queued')",
                (url_id, next_ordinal, 0),
            )
            con.execute(
                "INSERT INTO retry_transitions VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                (attempt_id, url_id, url, "external", "queued", None, None, None, 0, 0, 0, 0),
            )
            next_ordinal += 1
            added += 1
        con.execute(
            "UPDATE scan SET lifecycle='running',finished_at=NULL,finish_reason='retry_import',crawl_partial=1 WHERE singleton=1"
        )
        con.execute("DELETE FROM audit")
        con.commit()
        return {
            "scan": str(target),
            "backup": str(backup),
            "attempt_id": attempt_id,
            "imported": added,
            "rejected": rejected,
        }
    except BaseException:
        if con is not None:
            con.rollback()
        raise
    finally:
        if con is not None:
            con.close()
        from seohead.core.filesystem import unlock

        unlock(lock)
        os.close(lock)

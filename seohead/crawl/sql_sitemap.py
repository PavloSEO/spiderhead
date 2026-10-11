"""Bounded sitemap reconciliation over a read-only native scan.

This is deliberately not an analyzer bridge.  It recreates the native
``reconcile_sitemap`` populations with ephemeral file-backed SQLite tables so
the link graph never becomes a Python list or set.  The caller may consume
each output bucket as a cursor and decides how, or whether, it becomes an
existing audit finding.
"""

from __future__ import annotations

import contextlib
import itertools
import json
import sqlite3
from collections.abc import Iterator
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlsplit

from seohead.checks.parser import robots_directives
from seohead.checks.sitemap import normalize_url
from seohead.crawl.evidence import _same_page
from seohead.recon.net import normalize_url as normalize_start_url
from seohead.storage import ScanError, open_scan
from seohead.storage.analysis_graph import selected_links_cte

_BUCKETS = (
    "in_sitemap_and_linked",
    "in_sitemap_not_linked",
    "linked_not_in_sitemap",
    "linked_not_comparable",
)
_SERIAL = itertools.count()


class SitemapContextError(ValueError):
    """Saved sitemap context cannot support a truthful reconciliation."""


@dataclass
class SqlSitemapReconciliation:
    """Cursor-backed native sitemap result.  Call :meth:`close` when done."""

    _con: sqlite3.Connection | None
    _names: dict[str, str] = field(default_factory=dict)
    _owns_connection: bool = False
    _query_only_before: int | None = None
    available: bool = False
    reason: str = "no saved sitemap declarations"
    counts: dict[str, int] = field(default_factory=dict)
    crawl_partial: bool = False
    declared_raw_count: int = 0

    def iter_declared_facts(self) -> Iterator[sqlite3.Row]:
        """Stream declared URLs with their retained fetch, never a network probe."""
        if self._con is None or not self._names:
            return iter(())
        names = self._names
        return iter(
            self._con.execute(
                f"SELECT u.url,source.url AS sitemap_url,p.status_code,p.canonical,p.meta_robots,p.x_robots,"
                f"p.redirect_url,p.final_url,b.url_id IS NOT NULL AS blocked "
                f"FROM {names['declared']} d JOIN urls u ON u.url_id=d.report_url_id "
                "JOIN urls source ON source.url_id=d.sitemap_url_id "
                f"LEFT JOIN {names['pages']} n USING(key) LEFT JOIN pages p ON p.url_id=n.url_id "
                f"LEFT JOIN {names['blocked']} b ON b.url_id=d.report_url_id ORDER BY d.key"
            )
        )

    def iter_missing_canonical_pages(self) -> Iterator[sqlite3.Row]:
        """All fetched eligible pages absent from a complete declared population."""
        if not self.available or self._con is None:
            return iter(())
        names = self._names
        return iter(
            self._con.execute(
                f"SELECT u.url,p.status_code FROM {names['comparable']} c "
                f"LEFT JOIN {names['declared']} d USING(key) "
                "JOIN urls u ON u.url_id=c.report_url_id JOIN pages p ON p.url_id=u.url_id "
                "WHERE d.key IS NULL ORDER BY c.key"
            )
        )

    def iter_orphan_facts(self) -> Iterator[sqlite3.Row]:
        """Stream unlinked declarations with their actual selected sitemap source."""
        if not self.available or self._con is None:
            return iter(())
        names = self._names
        return iter(
            self._con.execute(
                f"SELECT u.url,source.url AS sitemap_url,p.status_code FROM {names['declared']} d "
                f"LEFT JOIN {names['observed']} o USING(key) JOIN urls u ON u.url_id=d.report_url_id "
                "JOIN urls source ON source.url_id=d.sitemap_url_id "
                f"LEFT JOIN {names['pages']} n USING(key) LEFT JOIN pages p ON p.url_id=n.url_id "
                "WHERE o.key IS NULL ORDER BY d.key"
            )
        )

    def count_bucket(self, name: str) -> int:
        """Count an output population without materializing it."""
        if name not in _BUCKETS:
            raise ValueError(f"unknown sitemap bucket: {name}")
        if not self.available:
            return 0
        if name == "in_sitemap_and_linked":
            return self.counts["urls_in_sitemap"] - self.counts["in_sitemap_not_in_crawl"]
        if name == "in_sitemap_not_linked":
            return self.counts["in_sitemap_not_in_crawl"]
        if name == "linked_not_in_sitemap":
            return self.counts["in_crawl_not_in_sitemap"]
        names = self._names
        return int(
            self._con.execute(
                f"SELECT COUNT(*) FROM {names['observed']} o "
                f"LEFT JOIN {names['comparable']} c USING(key) "
                f"LEFT JOIN {names['declared']} d USING(key) WHERE c.key IS NULL AND d.key IS NULL"
            ).fetchone()[0]
        )

    def iter_bucket(self, name: str) -> Iterator[str]:
        """Yield one stored spelling at a time in current normalized-key order."""
        if name not in _BUCKETS:
            raise ValueError(f"unknown sitemap bucket: {name}")
        if not self.available or self._con is None:
            return iter(())
        table = self._names
        sql = {
            "in_sitemap_and_linked": f"""
                SELECT u.url
                FROM {table["declared"]} AS d
                JOIN {table["observed"]} AS o USING(key)
                JOIN urls AS u ON u.url_id=o.report_url_id
                ORDER BY d.key COLLATE BINARY
            """,
            "in_sitemap_not_linked": f"""
                SELECT u.url
                FROM {table["declared"]} AS d
                LEFT JOIN {table["observed"]} AS o USING(key)
                JOIN urls AS u ON u.url_id=d.report_url_id
                WHERE o.key IS NULL
                ORDER BY d.key COLLATE BINARY
            """,
            "linked_not_in_sitemap": f"""
                SELECT u.url
                FROM {table["comparable"]} AS c
                JOIN {table["observed"]} AS o USING(key)
                LEFT JOIN {table["declared"]} AS d USING(key)
                JOIN urls AS u ON u.url_id=c.report_url_id
                WHERE d.key IS NULL
                ORDER BY c.key COLLATE BINARY
            """,
            "linked_not_comparable": f"""
                SELECT u.url
                FROM {table["observed"]} AS o
                LEFT JOIN {table["comparable"]} AS c USING(key)
                LEFT JOIN {table["declared"]} AS d USING(key)
                JOIN urls AS u ON u.url_id=o.report_url_id
                WHERE c.key IS NULL AND d.key IS NULL
                ORDER BY o.key COLLATE BINARY
            """,
        }[name]
        return (str(row[0]) for row in self._con.execute(sql))

    def summary(self) -> dict[str, Any]:
        """Return scalar reconciliation state without materializing any bucket."""
        data: dict[str, Any] = {"available": self.available, "reason": self.reason}
        if self.available:
            data.update(self.counts)
            data["crawl_partial"] = self.crawl_partial
        return data

    def materialize(self, max_items: int) -> dict[str, Any]:
        """Return a deliberately finite compatibility representation.

        The full graph remains cursor-backed; callers choosing this legacy
        bridge must name a positive item cap rather than accidentally turning a
        scan-size result into an unbounded Python list.
        """
        if type(max_items) is not int or max_items < 1:
            raise ValueError("max_items must be a positive integer")
        data: dict[str, Any] = {"available": self.available, "reason": self.reason}
        if not self.available:
            return data
        # Keep the legacy reconciliation's key order as well as its values:
        # the existing JSON report writer deliberately preserves input order.
        for name in ("urls_in_sitemap", "urls_reached_by_links"):
            data[name] = self.counts[name]
        for name in _BUCKETS:
            items = list(itertools.islice(self.iter_bucket(name), max_items + 1))
            if len(items) > max_items:
                raise ValueError(f"{name} exceeds materialize limit {max_items}")
            data[name] = items
        for name in ("in_sitemap_not_in_crawl", "in_crawl_not_in_sitemap"):
            data[name] = self.counts[name]
        data["crawl_partial"] = self.crawl_partial
        return data

    def close(self) -> None:
        if self._con is None:
            return
        with contextlib.suppress(sqlite3.Error):
            for table in self._names.values():
                self._con.execute(f"DROP TABLE IF EXISTS temp.{table}")
            if self._query_only_before is not None:
                self._con.execute(f"PRAGMA query_only={self._query_only_before}")
        if self._owns_connection:
            self._con.close()
        self._con = None

    def __enter__(self) -> SqlSitemapReconciliation:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def open_sitemap_reconciliation(path: str, *, start_url: str) -> SqlSitemapReconciliation:
    """Open a validated scan reader and prepare cursor-backed membership."""
    con = open_scan(path, require_audit=False)
    try:
        result = prepare_sitemap_reconciliation(con, start_url=start_url)
        result._owns_connection = True
        return result
    except BaseException:
        con.close()
        raise


def prepare_sitemap_reconciliation(
    con: sqlite3.Connection, *, start_url: str
) -> SqlSitemapReconciliation:
    """Prepare a result on an already validated ``open_scan`` connection.

    The caller owns the connection and keeps the one validated ``BEGIN``
    snapshot shared with the native audit path. ``open_scan`` configures the
    finite cache, progress handler, cancellation boundary, and file-backed
    temporary storage before that transaction starts.
    """
    query_only_before = int(con.execute("PRAGMA query_only").fetchone()[0])
    if con.execute("PRAGMA temp_store").fetchone()[0] != 1:
        raise ScanError("validated scan reader must configure file-backed temporary storage")
    if (
        con.execute(
            "SELECT 1 FROM context_items WHERE kind='sitemap_declaration' LIMIT 1"
        ).fetchone()
        is None
    ):
        return SqlSitemapReconciliation(
            con,
            reason="no saved sitemap declarations",
            _query_only_before=query_only_before,
        )
    names = _temp_names()
    # open_scan uses SQLite URI mode=ro, so this relaxation only permits
    # connection-local TEMP DDL; main-database writes remain impossible.
    if query_only_before:
        con.execute("PRAGMA query_only=OFF")
    _create_temp(con, names)
    _load_context_membership(con, names)
    _fill_declared(con, names)
    _fill_blocked(con, names["blocked"])
    _fill_comparable(con, names["comparable"], names["blocked"], start_url, names["pages"])
    if not _summaries_complete(con, names):
        return SqlSitemapReconciliation(
            con,
            names,
            reason="saved sitemap declaration membership is partial",
            _query_only_before=query_only_before,
        )
    _fill_observed(con, names["observed"])
    counts = _counts(con, names)
    crawl_partial = bool(
        con.execute("SELECT crawl_partial FROM scan WHERE singleton=1").fetchone()[0]
    )
    return SqlSitemapReconciliation(
        con,
        names,
        available=True,
        reason="",
        counts=counts,
        crawl_partial=crawl_partial,
        declared_raw_count=con.execute(
            f"SELECT COUNT(DISTINCT url_id) FROM {names['members']}"
        ).fetchone()[0],
        _query_only_before=query_only_before,
    )


def _load_context_membership(con: sqlite3.Connection, names: dict[str, str]) -> None:
    """Stream closed context records into temporary relational membership."""
    for row in _rows(
        con.execute(
            "SELECT item_key,payload_json FROM context_items "
            "WHERE kind='sitemap_declaration' ORDER BY item_key"
        )
    ):
        payload = _json_object(row["payload_json"], "sitemap_declaration")
        if set(payload) != {"sitemap_url_id", "source", "ordinal"}:
            raise SitemapContextError("saved sitemap declaration has an invalid payload")
        sitemap_url_id = payload["sitemap_url_id"]
        if type(sitemap_url_id) is not int or sitemap_url_id < 1:
            raise SitemapContextError("saved sitemap declaration has an invalid URL id")
        if (
            payload["source"] not in {"explicit", "robots", "fallback", "sitemap-only"}
            or type(payload["ordinal"]) is not int
        ):
            raise SitemapContextError("saved sitemap declaration has invalid source order")
        con.execute(f"INSERT OR IGNORE INTO {names['sources']} VALUES (?)", (sitemap_url_id,))
    for row in _rows(
        con.execute(
            "SELECT item_key,payload_json FROM context_items "
            "WHERE kind='sitemap_declared_url' ORDER BY item_key"
        )
    ):
        payload = _json_object(row["payload_json"], "sitemap_declared_url")
        if set(payload) != {"sitemap_url_id", "url_id", "ordinal"}:
            raise SitemapContextError("saved sitemap member has an invalid payload")
        sitemap_url_id, url_id, ordinal = (
            payload["sitemap_url_id"],
            payload["url_id"],
            payload["ordinal"],
        )
        if (
            type(sitemap_url_id) is not int
            or type(url_id) is not int
            or type(ordinal) is not int
            or sitemap_url_id < 1
            or url_id < 1
            or ordinal < 0
            or con.execute(
                f"SELECT 1 FROM {names['sources']} WHERE sitemap_url_id=?", (sitemap_url_id,)
            ).fetchone()
            is None
        ):
            raise SitemapContextError("saved sitemap member has invalid identity or ordinal")
        try:
            con.execute(
                f"INSERT INTO {names['members']} VALUES (?,?,?)", (ordinal, sitemap_url_id, url_id)
            )
        except sqlite3.IntegrityError as exc:
            raise SitemapContextError(
                "saved sitemap member ordinals are not globally unique"
            ) from exc
    for row in _rows(
        con.execute("SELECT payload_json FROM context_items WHERE kind='sitemap_fetch_summary'")
    ):
        payload = _json_object(row["payload_json"], "sitemap_fetch_summary")
        if set(payload) != {"sitemap_url_id", "response_ids", "complete", "reason"}:
            raise SitemapContextError("saved sitemap fetch summary has an invalid payload")
        sitemap_url_id = payload["sitemap_url_id"]
        if (
            type(sitemap_url_id) is not int
            or not isinstance(payload["response_ids"], list)
            or any(type(value) is not int or value < 1 for value in payload["response_ids"])
            or type(payload["complete"]) is not bool
            or type(payload["reason"]) is not str
            or con.execute(
                f"SELECT 1 FROM {names['sources']} WHERE sitemap_url_id=?", (sitemap_url_id,)
            ).fetchone()
            is None
        ):
            raise SitemapContextError("saved sitemap fetch summary has invalid identity")
        try:
            con.execute(
                f"INSERT INTO {names['summaries']} VALUES (?,?)",
                (sitemap_url_id, int(payload["complete"])),
            )
        except sqlite3.IntegrityError as exc:
            raise SitemapContextError("saved sitemap fetch summaries are duplicated") from exc


def _summaries_complete(con: sqlite3.Connection, names: dict[str, str]) -> bool:
    missing = con.execute(
        f"SELECT 1 FROM {names['sources']} AS source "
        f"LEFT JOIN {names['summaries']} AS summary USING(sitemap_url_id) "
        "WHERE summary.sitemap_url_id IS NULL OR summary.complete=0 LIMIT 1"
    ).fetchone()
    return missing is None


def _json_object(value: object, label: str) -> dict[str, Any]:
    try:
        parsed = json.loads(str(value))
    except (TypeError, ValueError) as exc:
        raise SitemapContextError(f"saved {label} is not JSON") from exc
    if not isinstance(parsed, dict):
        raise SitemapContextError(f"saved {label} is not an object")
    return parsed


def _rows(cursor: sqlite3.Cursor, size: int = 256) -> Iterator[sqlite3.Row]:
    """Read a bounded page before issuing writes on the same connection."""
    while batch := cursor.fetchmany(size):
        yield from batch


def _temp_names() -> dict[str, str]:
    serial = next(_SERIAL)
    prefix = f"e_sitemap_{serial}_"
    return {
        name: prefix + name
        for name in (
            "declared",
            "observed",
            "comparable",
            "blocked",
            "sources",
            "members",
            "summaries",
            "pages",
        )
    }


def _create_temp(con: sqlite3.Connection, names: dict[str, str]) -> None:
    for name in ("declared", "observed", "comparable"):
        extra = ",sitemap_url_id INTEGER NOT NULL" if name == "declared" else ""
        con.execute(
            f"CREATE TEMP TABLE {names[name]} "
            f"(key TEXT PRIMARY KEY, report_url_id INTEGER NOT NULL{extra}) WITHOUT ROWID"
        )
    con.execute(f"CREATE TEMP TABLE {names['blocked']} (url_id INTEGER PRIMARY KEY) WITHOUT ROWID")
    con.execute(
        f"CREATE TEMP TABLE {names['pages']} (key TEXT PRIMARY KEY,url_id INTEGER NOT NULL) WITHOUT ROWID"
    )
    con.execute(
        f"CREATE TEMP TABLE {names['sources']} (sitemap_url_id INTEGER PRIMARY KEY) WITHOUT ROWID"
    )
    con.execute(
        f"CREATE TEMP TABLE {names['members']} "
        "(ordinal INTEGER PRIMARY KEY, sitemap_url_id INTEGER NOT NULL, url_id INTEGER NOT NULL)"
    )
    con.execute(
        f"CREATE TEMP TABLE {names['summaries']} "
        "(sitemap_url_id INTEGER PRIMARY KEY, complete INTEGER NOT NULL CHECK(complete IN (0,1))) WITHOUT ROWID"
    )


def _normalized(url: str) -> str | None:
    try:
        return normalize_url(url)
    except ValueError:
        return None


def _fill_declared(con: sqlite3.Connection, names: dict[str, str]) -> None:
    for row in _rows(
        con.execute(
            f"SELECT m.url_id,m.sitemap_url_id,u.url FROM {names['members']} m "
            "LEFT JOIN urls u USING(url_id) ORDER BY m.ordinal"
        )
    ):
        url_id = int(row["url_id"])
        if row["url"] is None:
            raise SitemapContextError("saved sitemap member references an unknown URL")
        if key := _normalized(str(row["url"])):
            con.execute(
                f"INSERT OR IGNORE INTO {names['declared']} VALUES (?,?,?)",
                (key, url_id, row["sitemap_url_id"]),
            )


def _fill_observed(con: sqlite3.Connection, table: str) -> None:
    for row in _rows(
        con.execute(
            selected_links_cte("l") + "SELECT l.destination_url_id,destination.url "
            "FROM l JOIN pages AS source_page ON source_page.url_id=l.source_url_id "
            "JOIN urls AS destination ON destination.url_id=l.destination_url_id "
            "ORDER BY source_page.page_ordinal,l.ordinal,l.link_id"
        )
    ):
        if key := _normalized(str(row["url"])):
            con.execute(
                f"INSERT OR IGNORE INTO {table} VALUES (?,?)",
                (key, int(row["destination_url_id"])),
            )


def _fill_blocked(con: sqlite3.Connection, table: str) -> None:
    for row in _rows(
        con.execute(
            "SELECT payload_json FROM context_items WHERE kind='robots_blocked_url' ORDER BY item_key"
        )
    ):
        payload = _json_object(row["payload_json"], "robots-blocked URL")
        url_id = payload.get("url_id")
        if type(url_id) is not int or url_id < 1:
            raise SitemapContextError("saved robots-blocked URL has an invalid URL id")
        con.execute(f"INSERT OR IGNORE INTO {table} VALUES (?)", (url_id,))


def _fill_comparable(
    con: sqlite3.Connection, table: str, blocked: str, start_url: str, pages: str
) -> None:
    start_host = urlsplit(normalize_start_url(start_url)).netloc.lower()
    for row in _rows(
        con.execute(
            "SELECT p.url_id,u.url,p.status_code,p.content_type,p.meta_robots,p.x_robots,p.canonical,"
            "blocked.url_id IS NOT NULL AS blocked "
            "FROM pages AS p JOIN urls AS u ON u.url_id=p.url_id "
            f"LEFT JOIN {blocked} AS blocked ON blocked.url_id=p.url_id "
            "ORDER BY p.page_ordinal"
        )
    ):
        key = _normalized(str(row["url"]))
        if key:
            con.execute(f"INSERT OR IGNORE INTO {pages} VALUES (?,?)", (key, int(row["url_id"])))
        if row["blocked"]:
            continue
        status = row["status_code"]
        if status is None or not 200 <= int(status) < 300:
            continue
        if "html" not in str(row["content_type"] or "").lower():
            continue
        if start_host and urlsplit(str(row["url"])).netloc.lower() != start_host:
            continue
        if "noindex" in robots_directives(str(row["meta_robots"]), str(row["x_robots"])):
            continue
        if row["canonical"] and not _same_page(str(row["url"]), str(row["canonical"])):
            continue
        if key:
            con.execute(f"INSERT OR IGNORE INTO {table} VALUES (?,?)", (key, int(row["url_id"])))


def _counts(con: sqlite3.Connection, names: dict[str, str]) -> dict[str, int]:
    declared = con.execute(f"SELECT COUNT(*) FROM {names['declared']}").fetchone()[0]
    observed = con.execute(f"SELECT COUNT(*) FROM {names['observed']}").fetchone()[0]
    orphaned = con.execute(
        f"SELECT COUNT(*) FROM {names['declared']} AS d "
        f"LEFT JOIN {names['observed']} AS o USING(key) WHERE o.key IS NULL"
    ).fetchone()[0]
    missing = con.execute(
        f"SELECT COUNT(*) FROM {names['comparable']} AS c "
        f"JOIN {names['observed']} AS o USING(key) "
        f"LEFT JOIN {names['declared']} AS d USING(key) WHERE d.key IS NULL"
    ).fetchone()[0]
    return {
        "urls_in_sitemap": int(declared),
        "urls_reached_by_links": int(observed),
        "in_sitemap_not_in_crawl": int(orphaned),
        "in_crawl_not_in_sitemap": int(missing),
    }

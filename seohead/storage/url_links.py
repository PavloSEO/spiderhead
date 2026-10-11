"""Paged outgoing and incoming hyperlinks of one URL from a saved scan, read-only.

The lookup is by URL, not by document id: the URL is normalised the way the crawler keys pages
and resolved through the unique ``urls`` index. The scan is opened by a light read-only path (the
application id, a supported ``user_version``, the ``scan.v1``/``scan.v2`` header and the three
tables this query reads); it never runs a full-artifact validation, so opening costs the same on a
million-URL scan as on a small one. Pages are produced by index range scans with ``LIMIT/OFFSET``;
counts are capped and say so. Nothing is written.
"""

from __future__ import annotations

import json
import re
import sqlite3
import time
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from seohead.core.sqlite import open_readonly
from seohead.crawl.spider import Scope

from . import APPLICATION_ID, ScanError
from .link_queries import _spellings, _url

MAX_LIMIT = 200  # hard ceiling on rows per page
MAX_OFFSET = 5_000_000
COUNT_CAP = 1_000_000  # a count past this is reported as a lower bound
SCAN_CAP = 100_000  # rows a filtered/sorted request may examine before it reports a cap
_FRAGMENT_VARIANTS = 1_000  # fragment spellings of one destination looked up at most

LINK_TYPES = ("all", "internal", "external")
FOLLOW = ("all", "follow", "nofollow", "sponsored", "ugc")
STATUS_CLASSES = ("all", "2xx", "3xx", "4xx", "5xx", "broken", "error", "unscanned")
SORTS = ("order", "url", "status")
DIRECTIONS = ("out", "in")
_REPRESENTATIONS = ("all", "static", "rendered", "legacy_fragment", "legacy_unknown")

_NEEDED = {
    "scan": {"scan_uuid", "format_version", "start_url", "config_json", "capabilities_json"},
    "urls": {"url_id", "url"},
    "pages": {"url_id", "status_code"},
    "links": {
        "link_id",
        "source_url_id",
        "destination_url_id",
        "evidence_representation",
        "ordinal",
        "anchor",
        "nofollow",
        "position",
        "rel_json",
        "target",
        "raw_href",
    },
}
_LINK_COLUMNS = (
    "l.link_id,l.source_url_id,l.destination_url_id,l.evidence_representation,l.ordinal,"
    "l.anchor,l.nofollow,l.position,l.rel_json,l.target,l.raw_href"
)


class LinksError(ScanError):
    """A refusal with a machine-readable ``reason_code``."""

    def __init__(self, reason_code: str, message: str) -> None:
        super().__init__(message)
        self.reason_code = reason_code


class _Budget(Exception):
    """The time budget ended inside a statement."""


def _open_light(path: str | Path) -> sqlite3.Connection:
    """Read-only connection after the cheap identity checks; no ``quick_check``, no row scans."""
    target = Path(path)
    if not target.is_file():
        raise LinksError("scan_not_available", "scan file does not exist")
    con = None
    try:
        con = open_readonly(
            target.resolve(),
            row_factory=sqlite3.Row,
            pragmas=(
                "PRAGMA trusted_schema=OFF",
                "PRAGMA query_only=ON",
                "PRAGMA cache_size=-65536",
            ),
        )
        con.execute("BEGIN")
        version = con.execute("PRAGMA user_version").fetchone()[0]
        if con.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID:
            raise LinksError("scan_not_available", "not a SEOHEAD scan (application id differs)")
        if version not in {1, 2}:
            raise LinksError("scan_not_available", f"unsupported scan user_version {version}")
        for table, columns in _NEEDED.items():
            have = {row[1] for row in con.execute(f"PRAGMA table_info({table})")}
            if not columns <= have:
                raise LinksError("scan_not_available", f"scan has no usable {table} table")
        return con
    except sqlite3.Error as exc:
        if con is not None:
            con.close()
        raise LinksError("scan_not_available", f"cannot read scan: {exc}") from exc
    except BaseException:
        if con is not None:
            con.close()
        raise


def _header(con: sqlite3.Connection) -> tuple[dict[str, Any], Scope, str, str | None]:
    row = con.execute(
        "SELECT scan_uuid,format_version,evidence_revision,lifecycle,crawl_partial,start_url,"
        "config_json,capabilities_json,limitations_json FROM scan WHERE singleton=1"
    ).fetchone()
    if row is None or row["format_version"] not in {"scan.v1", "scan.v2"}:
        raise LinksError("scan_not_available", "scan header is missing or has another format")
    try:
        config = json.loads(row["config_json"])
        capabilities = json.loads(row["capabilities_json"])
        limitations = json.loads(row["limitations_json"])
        scope = Scope.from_config(config.get("scope") if isinstance(config, dict) else None)
    except (TypeError, ValueError, re.error) as exc:
        raise LinksError("scan_not_available", "scan header metadata is unreadable") from exc
    links = capabilities.get("links") if isinstance(capabilities, dict) else None
    links = links if isinstance(links, dict) else {}
    discovery = config.get("discovery") if isinstance(config, dict) else None
    discovery = discovery if isinstance(discovery, dict) else {}
    stored = {
        "internal": (discovery.get("hyperlinks") or {}).get("store", True) is not False,
        "external": (discovery.get("external") or {}).get("store", True) is not False,
    }
    reasons = []
    if row["lifecycle"] != "finished":
        reasons.append(f"scan lifecycle is {row['lifecycle']}")
    if row["crawl_partial"]:
        reasons.append("crawl is partial")
    if links.get("state") != "complete":
        reasons.append(links.get("reason") or "link capability is not complete")
    reasons.extend(r for r in limitations if str(r).startswith("link_observations_omitted"))
    if not stored["internal"]:
        reasons.append("internal links were not stored")
    if not stored["external"]:
        reasons.append("external links were not stored")
    unavailable = links.get("state") == "unavailable" or not (
        stored["internal"] or stored["external"]
    )
    start_host = (urlsplit(row["start_url"] or "").hostname or "").lower() or None
    header = {
        "scan_uuid": row["scan_uuid"],
        "evidence_revision": row["evidence_revision"],
        "coverage": {
            "state": "unavailable" if unavailable else "partial" if reasons else "complete",
            "reasons": reasons,
            "internal_links_stored": stored["internal"],
            "external_links_stored": stored["external"],
            "retained_attributes": ["rel", "target", "anchor", "position", "raw_href"],
            "not_retained_attributes": ["hreflang", "download"],
        },
    }
    return header, scope, start_host or "", row["start_url"]


def _ids(con: sqlite3.Connection, key: str, fragments: bool) -> list[int]:
    first, second = _spellings(key)
    sql = "SELECT url_id FROM urls WHERE url IN (?,?)"
    params: list[Any] = [first, second]
    if fragments:  # destination spellings carrying a fragment: url LIKE 'key#%'
        sql += " OR (url>=? AND url<?) OR (url>=? AND url<?)"
        params += [first + "#", first + "$", second + "#", second + "$"]
    sql += " LIMIT ?"
    return [r[0] for r in con.execute(sql, [*params, _FRAGMENT_VARIANTS + 2])]


def _status_class(scanned: bool, status: int | None) -> str:
    if not scanned:
        return "unscanned"
    if status is None:
        return "error"
    return f"{status // 100}xx" if 100 <= status < 600 else "error"


def _matches_status(want: str, cls: str) -> bool:
    return want == "all" or cls == want or (want == "broken" and cls in {"4xx", "5xx"})


def _flags(row: sqlite3.Row) -> dict[str, Any]:
    try:
        tokens = [str(t).lower() for t in json.loads(row["rel_json"])]
    except (TypeError, ValueError):
        tokens = []
    return {
        "rel": " ".join(tokens) or None,
        "nofollow": bool(row["nofollow"]),
        "sponsored": "sponsored" in tokens,
        "ugc": "ugc" in tokens,
    }


def _check(name: str, value: Any, allowed: tuple[str, ...]) -> None:
    if value not in allowed:
        raise LinksError("invalid_filter", f"{name} must be one of {', '.join(allowed)}")


class _Ctx:
    def __init__(self, con, scope, start_host, deadline):
        self.con, self.scope, self.start_host, self.deadline = con, scope, start_host, deadline
        self._base: dict[str, tuple[bool, int | None]] = {}

    def internal(self, url: str) -> bool | None:
        return self.scope.is_internal(url, self.start_host) if self.start_host else None

    def rows(self, sql: str, params: list[Any]):
        """Stream rows; a budget interrupt becomes ``_Budget`` wherever it fires."""
        try:
            yield from self.con.execute(sql, params)
        except sqlite3.OperationalError as exc:
            if "interrupted" in str(exc).lower():
                raise _Budget from exc
            raise

    def fetch(self, sql: str, params: list[Any]):
        return list(self.rows(sql, params))

    def base_status(self, url: str) -> tuple[bool, int | None]:
        """Status of the fragmentless page behind a destination that carries a fragment."""
        key = url.split("#", 1)[0] or url
        if key not in self._base:
            first, second = _spellings(_url(key, "link"))
            row = self.fetch(
                "SELECT p.status_code FROM urls u JOIN pages p ON p.url_id=u.url_id "
                "WHERE u.url IN (?,?) LIMIT 1",
                [first, second],
            )
            self._base[key] = (bool(row), row[0][0] if row else None)
        return self._base[key]


def _out_item(ctx: _Ctx, row: sqlite3.Row, index: int) -> dict[str, Any]:
    url = row["dest_url"]
    scanned = row["page_url_id"] is not None
    status = row["page_status"]
    if "#" in url:
        scanned, status = ctx.base_status(url)
    return {
        "index": index,
        "link_id": row["link_id"],
        "ordinal": row["ordinal"],
        "representation": row["evidence_representation"],
        "target_url": url,
        "anchor": row["anchor"],
        **_flags(row),
        "internal": ctx.internal(url),
        "target_scanned": scanned,
        "target_status": status if scanned else None,
        "position": row["position"] or None,
        "position_status": "classified" if row["position"] else "not_classified",
        "attributes": {"target": row["target"] or None},
        "raw_href": row["raw_href"] or None,
    }


def _in_item(ctx: _Ctx, row: sqlite3.Row, index: int) -> dict[str, Any]:
    url = row["src_url"]
    return {
        "index": index,
        "link_id": row["link_id"],
        "ordinal": row["ordinal"],
        "representation": row["evidence_representation"],
        "source_url": url,
        "target_url": row["dest_url"],
        "anchor": row["anchor"],
        **_flags(row),
        "internal": ctx.internal(url),
        "source_status": row["page_status"],
        "position": row["position"] or None,
        "position_status": "classified" if row["position"] else "not_classified",
        "attributes": {"target": row["target"] or None},
        "raw_href": row["raw_href"] or None,
    }


def _status_of(item: dict[str, Any], direction: str) -> str:
    if direction == "out":
        return _status_class(item["target_scanned"], item["target_status"])
    return _status_class(True, item["source_status"])


def _accepts(item: dict[str, Any], direction: str, link_type, status_class, contains) -> bool:
    if link_type != "all" and (
        item["internal"] is None or item["internal"] != (link_type == "internal")
    ):
        return False
    if not _matches_status(status_class, _status_of(item, direction)):
        return False
    if contains:
        other = item["target_url"] if direction == "out" else item["source_url"]
        if contains not in other.lower() and contains not in item["anchor"].lower():
            return False
    return True


def url_links(
    scan_path: str | Path,
    url: str,
    *,
    direction: str = "out",
    representation: str = "all",
    link_type: str = "all",
    follow: str = "all",
    status_class: str = "all",
    contains: str | None = None,
    sort: str = "order",
    offset: int = 0,
    limit: int = 100,
    timeout_seconds: float = 15.0,
    count_cap: int = COUNT_CAP,
    scan_cap: int = SCAN_CAP,
) -> dict[str, Any]:
    """One page of the outgoing (``out``) or incoming (``in``) links of a URL."""
    _check("direction", direction, DIRECTIONS)
    _check("representation", representation, _REPRESENTATIONS)
    _check("link_type", link_type, LINK_TYPES)
    _check("follow", follow, FOLLOW)
    _check("status_class", status_class, STATUS_CLASSES)
    _check("sort", sort, SORTS)
    if sort != "order" and direction == "in":
        raise LinksError("invalid_filter", "incoming links support only the default order")
    if contains is not None and (not isinstance(contains, str) or not 1 <= len(contains) <= 512):
        raise LinksError("invalid_filter", "contains must be 1..512 characters")
    if (
        type(limit) is not int
        or not 1 <= limit <= MAX_LIMIT
        or type(offset) is not int
        or not 0 <= offset <= MAX_OFFSET
        or type(timeout_seconds) not in {int, float}
        or not 0 < timeout_seconds <= 30
        or type(count_cap) is not int
        or not 1 <= count_cap <= 5_000_000
        or type(scan_cap) is not int
        or not 1 <= scan_cap <= 1_000_000
    ):
        raise LinksError(
            "invalid_page", f"invalid page; limit 1..{MAX_LIMIT}, offset 0..{MAX_OFFSET}"
        )
    key = _url(url, "url")
    needle = contains.lower() if contains else None
    deadline = time.monotonic() + timeout_seconds
    con = _open_light(scan_path)
    try:
        header, scope, start_host, _ = _header(con)
        ctx = _Ctx(con, scope, start_host, deadline)
        con.set_progress_handler(lambda: int(time.monotonic() >= deadline), 10_000)
        result: dict[str, Any] = {
            **header,
            "url": key,
            "direction": direction,
            "representation": representation,
            "filters": {
                "link_type": link_type,
                "follow": follow,
                "status_class": status_class,
                "contains": contains,
                "sort": sort,
            },
            "offset": offset,
            "limit": limit,
            "max_limit": MAX_LIMIT,
            "state": "complete",
            "reason_code": None,
            "items": [],
            "returned": 0,
            "has_more": False,
            "next_offset": None,
            "total": None,
            "total_capped": False,
            "filtered_total": None,
            "filtered_total_state": "unknown",
        }
        ids = _ids(con, key, fragments=direction == "in")
        if not ids:
            raise LinksError("url_not_found", "URL is not recorded in this scan")
        if len(ids) > _FRAGMENT_VARIANTS:
            raise LinksError("too_many_variants", "URL has too many fragment spellings")
        page = con.execute(
            f"SELECT status_code FROM pages WHERE url_id IN ({','.join('?' * len(ids))}) LIMIT 1",
            ids,
        ).fetchone()
        result["page"] = {"scanned": page is not None, "status": page[0] if page else None}
        if direction == "out" and page is None:
            return {**result, "state": "unavailable", "reason_code": "url_not_scanned"}
        if header["coverage"]["state"] == "unavailable":
            return {**result, "state": "unavailable", "reason_code": "links_not_retained"}
        try:
            _page(
                ctx,
                result,
                ids,
                representation,
                (link_type, follow, status_class, needle, sort, count_cap, scan_cap),
            )
        except _Budget:
            result.update(state="limit_reached", reason_code="time_budget_exhausted")
        coverage = header["coverage"]
        if result["total"] == 0 and not (
            coverage["internal_links_stored"] and coverage["external_links_stored"]
        ):
            result.update(state="unavailable", reason_code="links_not_retained")
        return result
    finally:
        con.close()


def _page(ctx, result, ids, representation, req) -> None:
    link_type, follow, status_class, needle, sort, count_cap, scan_cap = req
    direction, offset, limit = result["direction"], result["offset"], result["limit"]
    marks = ",".join("?" * len(ids))
    where, params = [], []
    if direction == "out":
        where.append(f"l.source_url_id IN ({marks})")
    else:
        where.append(f"l.destination_url_id IN ({marks})")
    params += ids
    if representation != "all":
        where.append("l.evidence_representation=?")
        params.append(representation)
    base_where, base_params = " AND ".join(where), list(params)
    if follow in ("sponsored", "ugc"):
        # Exact token match on the stored rel list; `json_each` avoids LIKE false hits.
        where.append("EXISTS (SELECT 1 FROM json_each(l.rel_json) WHERE value=?)")
        params.append(follow)
    elif follow != "all":
        where.append("l.nofollow=?")
        params.append(1 if follow == "nofollow" else 0)
    flt_where = " AND ".join(where)
    py_filtering = link_type != "all" or status_class != "all" or needle or sort != "order"

    if direction == "out":
        select = (
            f"SELECT {_LINK_COLUMNS},d.url AS dest_url,p.url_id AS page_url_id,"
            "p.status_code AS page_status FROM links l "
            "JOIN urls d ON d.url_id=l.destination_url_id "
            "LEFT JOIN pages p ON p.url_id=l.destination_url_id"
        )
        order = "l.evidence_representation,l.ordinal,l.link_id"
        make = _out_item
    else:
        select = (
            f"SELECT {_LINK_COLUMNS},s.url AS src_url,d.url AS dest_url,"
            "p.status_code AS page_status FROM links l "
            "JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id "
            "LEFT JOIN pages p ON p.url_id=l.source_url_id"
        )
        order = "l.destination_url_id,l.source_url_id,l.position,l.link_id"
        make = _in_item
    index_hint = "" if direction == "out" else " INDEXED BY links_destination_source_position"

    def counts(where_sql, where_params, cap):
        inner = f"SELECT l.source_url_id FROM links l{index_hint} WHERE {where_sql} LIMIT ?"
        row = ctx.fetch(
            f"SELECT COUNT(*),COUNT(DISTINCT source_url_id) FROM ({inner})",
            [*where_params, cap + 1],
        )[0]
        return (min(row[0], cap), row[0] > cap, None if row[0] > cap else row[1])

    items: list[dict[str, Any]] = []
    if not py_filtering:
        sql = (
            f"{select} WHERE l.link_id IN (SELECT l.link_id FROM links l{index_hint} "
            f"WHERE {flt_where} ORDER BY {order} LIMIT ? OFFSET ?) ORDER BY {order}"
        )
        rows = ctx.fetch(sql, [*params, limit + 1, offset])
        for number, row in enumerate(rows[:limit], offset + 1):
            items.append(make(ctx, row, number))
        has_more = len(rows) > limit
    else:
        # Filters that need the other end's host/status, or a re-sort, examine the URL's own links
        # in order (all of them are bounded per URL) up to scan_cap rows.
        cursor = ctx.rows(f"{select} WHERE {flt_where} ORDER BY {order}", params)
        matched: list[dict[str, Any]] = []
        exhausted = True
        for examined, row in enumerate(cursor):
            if examined >= scan_cap:
                exhausted = False
                break
            item = make(ctx, row, 0)
            if _accepts(item, direction, link_type, status_class, needle):
                matched.append(item)
        if sort == "url":
            matched.sort(key=lambda i: (i.get("target_url") or "", i["link_id"]))
        elif sort == "status":
            matched.sort(
                key=lambda i: (
                    i["target_status"] is None,
                    i["target_status"] or 0,
                    i["link_id"],
                )
            )
        window = matched[offset : offset + limit + 1]
        for number, item in enumerate(window[:limit], offset + 1):
            item["index"] = number
            items.append(item)
        has_more = len(window) > limit or not exhausted
        result["filtered_total"] = len(matched) if exhausted else None
        result["filtered_total_state"] = "exact" if exhausted else "scan_capped"
        if not exhausted:
            result["reason_code"] = "filter_scan_capped"
            result["state"] = "partial"
    result["items"], result["returned"], result["has_more"] = items, len(items), has_more
    result["next_offset"] = offset + len(items) if has_more else None
    # Counts last, on whatever time is left: a page is worth more than its total.
    try:
        total, capped, unique = counts(base_where, base_params, count_cap)
        result["total"], result["total_capped"] = total, capped
        if direction == "in":
            result["unique_sources"] = unique
        if not py_filtering:
            if follow == "all":
                result["filtered_total"], result["filtered_total_state"] = (
                    total,
                    ("capped" if capped else "exact"),
                )
            else:
                ft, fcapped, _ = counts(flt_where, params, count_cap)
                result["filtered_total"] = ft
                result["filtered_total_state"] = "capped" if fcapped else "exact"
        if direction == "out":
            result["summary"] = (
                None
                if capped or total > scan_cap
                else _out_summary(ctx, base_where, base_params, scan_cap)
            )
    except _Budget:
        result["total_state"] = "time_budget_exhausted"
        if not py_filtering:
            result["filtered_total_state"] = "time_budget_exhausted"


def _out_summary(ctx, where, params, scan_cap) -> dict[str, Any] | None:
    """Counts of internal/external/nofollow links of the URL, or None when too many to scan."""
    rows = ctx.rows(
        "SELECT d.url,l.nofollow FROM links l JOIN urls d ON d.url_id=l.destination_url_id "
        f"WHERE {where}",
        params,
    )
    total = internal = external = nofollow = unknown = 0
    for url, nf in rows:
        if total >= scan_cap:
            return None
        total += 1
        nofollow += 1 if nf else 0
        side = ctx.internal(url)
        if side is None:
            unknown += 1
        elif side:
            internal += 1
        else:
            external += 1
    return {
        "total": total,
        "internal": internal,
        "external": external,
        "unknown_locality": unknown,
        "nofollow": nofollow,
    }

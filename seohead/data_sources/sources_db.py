"""Accumulate GA4, Metrika, Search Console and Yandex Webmaster data in one local SQLite file.

Provider envelopes are one-off: every trend question would re-download the whole period, and
Search Console forgets anything older than 16 months. This module keeps one database per
project instead:

* one table per source, keyed by ``(resource, date, dimensions...)``. A synced day is replaced
  atomically (delete + insert in one transaction), so re-syncing never duplicates rows;
* a ``sync_log`` ledger records every fetched ``(source, resource, date)``, including days that
  returned no rows, so a sync fetches only missing or incomplete days;
* each source has a data lag (Search Console finalizes ~3 days late), and days inside that lag
  are never marked as synced.

Only resource identifiers (property, counter, host) are stored; tokens never reach the file.
"""

from __future__ import annotations

import csv
import math
import os
import sqlite3
import time
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from datetime import date, datetime, timedelta, timezone
from itertools import pairwise
from pathlib import Path
from typing import Any

from seohead.core.sqlite import open_readonly, open_writer

DB_NAME = "sources.sqlite"
SCHEMA_VERSION = "1"
SCHEMA_KIND = "seohead.sources"
RETRIES = 2
RETRY_PAUSE = 2.0
MAX_DAYS = 800
MAX_EXPORT_ROWS = 100_000

Fetcher = Callable[[str, str, str], dict[str, Any]]


@dataclass(frozen=True)
class Source:
    table: str
    dimensions: tuple[str, ...]
    metrics: tuple[str, ...]
    lag_days: int
    chunk_days: int
    resource_hint: str
    non_additive: tuple[str, ...] = ()
    date_timezone: str = "provider_resource"


SOURCES: dict[str, Source] = {
    "gsc": Source(
        "gsc",
        ("query", "page"),
        ("clicks", "impressions", "ctr", "position"),
        3,
        1,
        "sc-domain:…",
        ("ctr", "position"),
        "America/Los_Angeles",
    ),
    "ga4": Source(
        "ga4",
        ("landing_page", "channel"),
        ("sessions", "engaged_sessions", "users"),
        1,
        31,
        "GA4 property ID",
        ("users",),
    ),
    "metrika": Source(
        "metrika",
        ("traffic_source", "landing_page"),
        ("visits", "users", "bounce_rate"),
        1,
        31,
        "Metrika counter ID",
        ("users", "bounce_rate"),
    ),
    "webmaster": Source(
        "webmaster",
        ("query",),
        ("shows", "clicks", "show_position", "click_position"),
        3,
        1,
        "Webmaster host ID, e.g. https:example.com:443",
        ("show_position", "click_position"),
    ),
    "webmaster_history": Source(
        "webmaster_history",
        ("indicator",),
        ("value",),
        3,
        31,
        "Webmaster host ID, e.g. https:example.com:443",
        ("value",),
    ),
}


# --- database ----------------------------------------------------------------


def db_path(db: str | None = None, project: str | None = None) -> Path:
    """Resolve an explicit database path, or ``<project>/sources.sqlite``."""
    if bool(db) == bool(project):
        raise ValueError("pass exactly one of db or project")
    if project:
        root = Path(project)
        if not (root / "project.json").is_file():
            raise ValueError("project must be a seohead project directory with project.json")
        return root / DB_NAME
    return Path(str(db))


def connect(path: Path, *, create: bool) -> sqlite3.Connection:
    if not create and not path.is_file():
        raise ValueError(f"no sources database at {path}; run sources-sync first")
    if create and not path.exists():
        try:
            descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        except FileExistsError:
            pass
        else:
            os.close(descriptor)
    connection = open_writer(path) if create else open_readonly(path.resolve())
    if create:
        connection.execute("BEGIN IMMEDIATE")
    marker = connection.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='sources_meta'"
    ).fetchone()
    if marker:
        identity = dict(connection.execute("SELECT key, value FROM sources_meta"))
        if identity.get("kind") != SCHEMA_KIND or identity.get("schema_version") != SCHEMA_VERSION:
            if create:
                connection.rollback()
            connection.close()
            raise ValueError("unsupported sources database identity or schema version")
        if not create:
            connection.execute("PRAGMA query_only=ON")
        else:
            connection.commit()
        return connection
    elif not create or path.stat().st_size:
        if create:
            connection.rollback()
        connection.close()
        raise ValueError("existing database is not a versioned sources database; back it up first")
    connection.execute("CREATE TABLE sources_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
    connection.executemany(
        "INSERT INTO sources_meta(key,value) VALUES(?,?)",
        (("kind", SCHEMA_KIND), ("schema_version", SCHEMA_VERSION)),
    )
    connection.execute(
        "CREATE TABLE IF NOT EXISTS sync_log (source TEXT NOT NULL, resource TEXT NOT NULL,"
        " date TEXT NOT NULL, rows INTEGER NOT NULL DEFAULT 0, complete INTEGER NOT NULL DEFAULT 0,"
        " fetched_at TEXT, state TEXT NOT NULL, last_attempt_state TEXT,"
        " last_attempt_at TEXT, PRIMARY KEY (source, resource, date))"
    )
    for spec in SOURCES.values():
        dims = "".join(f", {name} TEXT NOT NULL DEFAULT ''" for name in spec.dimensions)
        metrics = "".join(f", {name} REAL" for name in spec.metrics)
        keys = ", ".join(("resource", "date", *spec.dimensions))
        connection.execute(
            f"CREATE TABLE IF NOT EXISTS {spec.table} (resource TEXT NOT NULL,"
            f" date TEXT NOT NULL{dims}{metrics}, PRIMARY KEY ({keys}))"
        )
    connection.commit()
    return connection


def _store(
    connection: sqlite3.Connection,
    source: str,
    resource: str,
    days: list[str],
    rows: list[dict[str, Any]],
    complete: bool,
) -> int:
    spec = SOURCES[source]
    columns = ("resource", "date", *spec.dimensions, *spec.metrics)
    fetched_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
    day_rows: dict[str, dict[tuple[str, ...], tuple[Any, ...]]] = {day: {} for day in days}
    for row in rows:
        day = row.get("date")
        if day not in day_rows:
            raise ValueError("provider row date is outside the requested period")
        if any(name not in row or row[name] is None for name in spec.dimensions):
            raise ValueError("provider row is missing a dimension value")
        key = tuple(str(row[name]) for name in spec.dimensions)
        if key in day_rows[day]:
            raise ValueError("provider returned duplicate normalized dimension keys")
        day_rows[day][key] = (
            resource,
            day,
            *key,
            *(_number(row.get(name)) for name in spec.metrics),
        )
    stored = 0
    for day in days:
        values = list(day_rows[day].values())
        state = "partial" if not complete else ("complete" if values else "empty")
        # Each calendar day is a transaction. A failed insert leaves its previous facts and
        # coverage intact; an incomplete retry cannot erase already complete evidence.
        with connection:
            connection.execute("BEGIN IMMEDIATE")
            prior = connection.execute(
                "SELECT complete FROM sync_log WHERE source=? AND resource=? AND date=?",
                (source, resource, day),
            ).fetchone()
            if prior and prior[0] and not complete:
                connection.execute(
                    "UPDATE sync_log SET last_attempt_state=?, last_attempt_at=?"
                    " WHERE source=? AND resource=? AND date=?",
                    (state, fetched_at, source, resource, day),
                )
                continue
            connection.execute(
                f"DELETE FROM {spec.table} WHERE resource = ? AND date = ?", (resource, day)
            )
            connection.executemany(
                f"INSERT INTO {spec.table} ({', '.join(columns)})"
                f" VALUES ({', '.join('?' * len(columns))})",
                values,
            )
            connection.execute(
                "INSERT OR REPLACE INTO sync_log"
                " (source,resource,date,rows,complete,fetched_at,state,last_attempt_state,last_attempt_at)"
                " VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    source,
                    resource,
                    day,
                    len(values),
                    int(complete),
                    fetched_at,
                    state,
                    state,
                    fetched_at,
                ),
            )
            stored += len(values)
    return stored


def _mark_requested(
    connection: sqlite3.Connection, source: str, resource: str, days: list[str], available_end: str
) -> None:
    with connection:
        for day in days:
            state = "lagged" if day > available_end else "pending"
            connection.execute(
                "INSERT OR IGNORE INTO sync_log(source,resource,date,state) VALUES(?,?,?,?)",
                (source, resource, day, state),
            )
            if state == "lagged":
                connection.execute(
                    "UPDATE sync_log SET state='lagged' WHERE source=? AND resource=? AND date=?"
                    " AND complete=0 AND state IN ('lagged','pending')",
                    (source, resource, day),
                )
            else:
                connection.execute(
                    "UPDATE sync_log SET state='pending' WHERE source=? AND resource=? AND date=?"
                    " AND complete=0 AND state='lagged'",
                    (source, resource, day),
                )


def _mark_failed(
    connection: sqlite3.Connection,
    source: str,
    resource: str,
    days: list[str],
    *,
    attempt_state: str = "failed",
) -> None:
    if attempt_state not in {"failed", "no_access", "invalid_grant", "insufficient_scope"}:
        attempt_state = "failed"
    at = datetime.now(timezone.utc).isoformat(timespec="seconds")
    with connection:
        for day in days:
            connection.execute(
                "UPDATE sync_log SET state=CASE WHEN complete=1 THEN state ELSE 'failed' END,"
                " last_attempt_state=?, last_attempt_at=?"
                " WHERE source=? AND resource=? AND date=?",
                (attempt_state, at, source, resource, day),
            )


def _failure_state(result: dict[str, Any]) -> str:
    """Retain only explicit access-failure classes, never provider error text."""
    error = str(result.get("error") or "").casefold()
    if "invalid_grant" in error:
        return "invalid_grant"
    if "insufficient_scope" in error or "insufficient scope" in error:
        return "insufficient_scope"
    if result.get("status") in (401, 403):
        return "no_access"
    return "failed"


def _number(value: Any) -> float | None:
    try:
        if value is None or value == "":
            return None
        if isinstance(value, bool):
            raise ValueError
        number = float(value)
        if not math.isfinite(number):
            raise ValueError("provider metric must be finite")
        return number
    except (TypeError, ValueError):
        raise ValueError("provider metric must be numeric and finite") from None


# --- dates -------------------------------------------------------------------


def _days(start: str, end: str) -> list[str]:
    first, last = date.fromisoformat(start), date.fromisoformat(end)
    if first > last:
        raise ValueError("start_date must not be after end_date")
    return [(first + timedelta(days=n)).isoformat() for n in range((last - first).days + 1)]


def _chunks(days: list[str], size: int) -> Iterable[list[str]]:
    """Group sorted days into contiguous runs of at most ``size`` days."""
    run: list[str] = []
    for day in days:
        contiguous = run and date.fromisoformat(day) - date.fromisoformat(run[-1]) == timedelta(1)
        if run and (not contiguous or len(run) >= size):
            yield run
            run = []
        run.append(day)
    if run:
        yield run


def _gaps(days: list[str]) -> list[list[str]]:
    """Return missing ``[start, end]`` ranges between the first and last synced day."""
    gaps = []
    for previous, current in pairwise(days):
        before, after = date.fromisoformat(previous), date.fromisoformat(current)
        if after - before > timedelta(1):
            gaps.append([(before + timedelta(1)).isoformat(), (after - timedelta(1)).isoformat()])
    return gaps


def available_end(source: str, *, as_of: date | None = None) -> date:
    """Return the latest safely finalized date used by both sync and readiness.

    ``as_of`` is the source calendar date to treat as today: the provider-local
    date for a known timezone, otherwise the UTC date. Unknown resource timezones
    conservatively defer one extra day.
    """
    from zoneinfo import ZoneInfo

    spec = _source(source)
    if as_of is None:
        today = (
            datetime.now(ZoneInfo(spec.date_timezone)).date()
            if spec.date_timezone == "America/Los_Angeles"
            else datetime.now(timezone.utc).date()
        )
    else:
        today = as_of
    if spec.date_timezone != "America/Los_Angeles":
        today -= timedelta(days=1)
    return today - timedelta(days=spec.lag_days)


def _requested_period(
    source: str, start_date: str | None, end_date: str | None
) -> tuple[str, str, str]:
    """Keep the requested window intact while identifying the provider's lag boundary."""
    available = available_end(source)
    end = date.fromisoformat(end_date) if end_date else available
    start = date.fromisoformat(start_date) if start_date else end - timedelta(days=27)
    if start > end:
        raise ValueError("start_date must not be after end_date")
    if (end - start).days + 1 > MAX_DAYS:
        raise ValueError(f"a sync period is limited to {MAX_DAYS} days")
    return start.isoformat(), end.isoformat(), available.isoformat()


# --- provider fetchers ----------------------------------------------------------
# Each fetcher returns {"ok", "rows": [{"date", dims..., metrics...}], "complete", "error"}.


def _fetch_gsc(resource: str, start: str, end: str) -> dict[str, Any]:
    """Fetch a bounded daily GSC web grain through the current provider adapter."""
    from seohead.data_sources import gsc

    result = gsc.search_analytics_pages(
        resource,
        start_date=start,
        end_date=end,
        dimensions=["date", "query", "page"],
        row_limit=1_000,
        max_rows=gsc.MAX_ANALYTICS_ROWS,
    )
    if not result.get("ok"):
        return _failure(result)
    rows = []
    for row in result["rows"]:
        keys = row.get("keys")
        if not isinstance(keys, list) or len(keys) != 3:
            return {"ok": False, "error": "malformed GSC dimension keys", "retry": False}
        day, query, url = keys
        rows.append(
            {
                "date": day,
                "query": query,
                "page": url,
                **{
                    metric: row.get(metric)
                    for metric in ("clicks", "impressions", "ctr", "position")
                },
            }
        )
    return {
        "ok": True,
        "rows": rows,
        "complete": result.get("state", "complete") == "complete" and not result.get("truncated"),
    }


def _fetch_ga4(resource: str, start: str, end: str) -> dict[str, Any]:
    from seohead.data_sources import ga4

    result = ga4.run_report(
        resource,
        start,
        end,
        ["date", "landingPagePlusQueryString", "sessionDefaultChannelGroup"],
        ["sessions", "engagedSessions", "totalUsers"],
    )
    if not result.get("ok"):
        return _failure(result)
    rows = [
        {
            "date": f"{row['date'][:4]}-{row['date'][4:6]}-{row['date'][6:8]}",
            "landing_page": row.get("landingPagePlusQueryString"),
            "channel": row.get("sessionDefaultChannelGroup"),
            "sessions": row.get("sessions"),
            "engaged_sessions": row.get("engagedSessions"),
            "users": row.get("totalUsers"),
        }
        for row in result["rows"]
    ]
    return {"ok": True, "rows": rows, "complete": result.get("state") == "complete"}


def _fetch_metrika(resource: str, start: str, end: str) -> dict[str, Any]:
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.metrika import MetrikaClient, MetrikaError

    params = {
        "ids": resource,
        "date1": start,
        "date2": end,
        "dimensions": "ym:s:date,ym:s:lastTrafficSource,ym:s:startURL",
        "metrics": "ym:s:visits,ym:s:users,ym:s:bounceRate",
    }
    try:
        body = MetrikaClient().report(params, paginate=True, limit=1000)
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except MetrikaError as exc:
        return {"ok": False, "error": exc.message, "status": exc.status}
    data = body.get("data") if isinstance(body, dict) else None
    if not isinstance(data, list):
        return {"ok": False, "error": "malformed Metrika report data", "retry": False}
    rows = []
    for row in data:
        if not isinstance(row, dict):
            return {"ok": False, "error": "malformed Metrika report row", "retry": False}
        dimensions = row.get("dimensions")
        metrics = row.get("metrics")
        if (
            not isinstance(dimensions, list)
            or len(dimensions) != 3
            or not all(isinstance(item, dict) and "name" in item for item in dimensions)
            or not isinstance(metrics, list)
            or len(metrics) != 3
        ):
            return {"ok": False, "error": "malformed Metrika report row", "retry": False}
        dims = [item["name"] for item in dimensions]
        rows.append(
            {
                "date": dims[0],
                "traffic_source": dims[1],
                "landing_page": dims[2],
                "visits": metrics[0],
                "users": metrics[1],
                "bounce_rate": metrics[2],
            }
        )
    complete = not body.get("capped") and not body.get("sampled")
    return {"ok": True, "rows": rows, "complete": complete}


def _webmaster(
    operation: str, host: str, params: dict[str, Any], *, paginate: bool = False
) -> dict[str, Any]:
    """Use the current read-only Webmaster adapter and its bounded pagination."""
    from seohead.data_sources import yandex_webmaster as wm

    return wm.collect(operation, host_id=host, params=params, paginate=paginate)


def _failure(result: dict[str, Any]) -> dict[str, Any]:
    """Retry transient failures, but do not loop on a quota refusal or a client error."""
    status = result.get("status")
    retry = not isinstance(status, int) or status >= 500
    return {
        "ok": False,
        "error": result.get("error"),
        "status": status,
        "retry": retry,
    }


_WEBMASTER_INDICATORS = ["TOTAL_SHOWS", "TOTAL_CLICKS", "AVG_SHOW_POSITION", "AVG_CLICK_POSITION"]


def _fetch_webmaster(resource: str, start: str, end: str) -> dict[str, Any]:
    rows: list[dict[str, Any]] = []
    complete = True
    for day in _days(start, end):
        result = _webmaster(
            "search_performance",
            resource,
            {"query_indicator": _WEBMASTER_INDICATORS, "date_from": day, "date_to": day},
            paginate=True,
        )
        if not result.get("ok"):
            return _failure(result)
        queries = result["data"].get("queries")
        count = result["data"].get("count")
        if (
            not isinstance(queries, list)
            or not isinstance(count, int)
            or isinstance(count, bool)
            or count < len(queries)
            or not all(isinstance(item, dict) for item in queries)
        ):
            return {"ok": False, "error": "malformed Webmaster queries", "retry": False}
        complete = complete and not result.get("truncated", False) and count == len(queries)
        for item in queries:
            values = item.get("indicators") or {}
            if not isinstance(values, dict) or item.get("query_text") is None:
                return {"ok": False, "error": "malformed Webmaster query row", "retry": False}
            rows.append(
                {
                    "date": day,
                    "query": item.get("query_text"),
                    "shows": values.get("TOTAL_SHOWS"),
                    "clicks": values.get("TOTAL_CLICKS"),
                    "show_position": values.get("AVG_SHOW_POSITION"),
                    "click_position": values.get("AVG_CLICK_POSITION"),
                }
            )
    return {"ok": True, "rows": rows, "complete": complete}


def _fetch_webmaster_history(resource: str, start: str, end: str) -> dict[str, Any]:
    """Daily totals: shows/clicks, pages in search, and crawled pages by HTTP class."""
    period = {"date_from": start, "date_to": end}
    totals: dict[tuple[str, str], float] = {}
    for operation, extra in (
        ("search_history", {"query_indicator": ["TOTAL_SHOWS", "TOTAL_CLICKS"]}),
        ("indexing_history", {}),
        ("in_search_history", {}),
    ):
        result = _webmaster(operation, resource, dict(period, **extra))
        if not result.get("ok"):
            return _failure(result)
        data = result["data"]
        if operation == "in_search_history":
            history = data.get("history")
            if not isinstance(history, list):
                return {"ok": False, "error": "malformed Webmaster history", "retry": False}
            series = {"IN_SEARCH": history}
        else:
            series = data.get("indicators")
            if not isinstance(series, dict):
                return {"ok": False, "error": "malformed Webmaster indicators", "retry": False}
        for indicator, points in series.items():
            if not isinstance(points, list):
                return {"ok": False, "error": "malformed Webmaster points", "retry": False}
            for point in points:
                if not isinstance(point, dict) or "date" not in point or "value" not in point:
                    return {"ok": False, "error": "malformed Webmaster point", "retry": False}
                key = (str(point["date"])[:10], indicator)
                try:
                    value = _number(point["value"])
                except ValueError:
                    return {"ok": False, "error": "malformed Webmaster value", "retry": False}
                if value is None:
                    return {"ok": False, "error": "missing Webmaster value", "retry": False}
                # Crawl counts arrive in several intra-day batches and are summed; a pages-in-
                # search snapshot is a level, so the day's latest value is kept.
                if operation == "indexing_history":
                    totals[key] = totals.get(key, 0.0) + value
                else:
                    totals[key] = value
    rows = [{"date": d, "indicator": i, "value": v} for (d, i), v in sorted(totals.items())]
    return {"ok": True, "rows": rows, "complete": True}


FETCHERS: dict[str, Fetcher] = {
    "gsc": _fetch_gsc,
    "ga4": _fetch_ga4,
    "metrika": _fetch_metrika,
    "webmaster": _fetch_webmaster,
    "webmaster_history": _fetch_webmaster_history,
}


# --- public operations ---------------------------------------------------------


def _source(source: str) -> Source:
    if source not in SOURCES:
        raise ValueError(f"source must be one of {sorted(SOURCES)}")
    return SOURCES[source]


def sync(
    path: Path,
    source: str,
    resource: str,
    *,
    start_date: str | None = None,
    end_date: str | None = None,
    force: bool = False,
    fetchers: dict[str, Fetcher] | None = None,
) -> dict[str, Any]:
    """Fetch missing or incomplete days for one source/resource and store them."""
    spec = _source(source)
    if not resource or not isinstance(resource, str):
        raise ValueError(f"resource is required ({spec.resource_hint})")
    start, end, available_end = _requested_period(source, start_date, end_date)
    path.parent.mkdir(parents=True, exist_ok=True)
    connection = connect(path, create=True)
    try:
        requested = _days(start, end)
        _mark_requested(connection, source, resource, requested, available_end)
        available = [day for day in requested if day <= available_end]
        lagged = [day for day in requested if day > available_end]
        done = {
            day
            for (day,) in connection.execute(
                "SELECT date FROM sync_log WHERE source = ? AND resource = ? AND complete = 1"
                " AND date BETWEEN ? AND ?",
                (source, resource, start, end),
            )
        }
        missing = [day for day in available if force or day not in done]
        fetch = (fetchers or FETCHERS)[source]
        synced = attempted = rows_stored = 0
        incomplete: list[str] = []
        for chunk in _chunks(missing, spec.chunk_days):
            attempted += len(chunk)
            for attempt in range(RETRIES + 1):
                try:
                    result = fetch(resource, chunk[0], chunk[-1])
                except Exception as exc:
                    result = {
                        "ok": False,
                        "retry": False,
                        "error": f"provider adapter raised {type(exc).__name__}",
                    }
                if not isinstance(result, dict):
                    result = {
                        "ok": False,
                        "retry": False,
                        "error": "provider adapter returned an invalid result",
                    }
                if result.get("ok") or result.get("retry") is False:
                    break
                if attempt < RETRIES:
                    time.sleep(RETRY_PAUSE * (attempt + 1))
            if not result.get("ok"):
                _mark_failed(
                    connection,
                    source,
                    resource,
                    chunk,
                    attempt_state=_failure_state(result),
                )
                return {
                    "ok": False,
                    "source": source,
                    "resource": resource,
                    "error": result.get("error") or "provider request failed",
                    "failed_period": [chunk[0], chunk[-1]],
                    "synced_days": synced,
                    "attempted_days": attempted,
                    "rows_stored": rows_stored,
                    "requested_period": [start, end],
                    "available_period": [start, min(end, available_end)] if available else None,
                    "lagged_days": lagged,
                    "db": str(path),
                }
            if not isinstance(result.get("rows"), list) or not isinstance(
                result.get("complete"), bool
            ):
                _mark_failed(connection, source, resource, chunk)
                return {
                    "ok": False,
                    "source": source,
                    "resource": resource,
                    "error": "provider fetcher must return rows list and complete boolean",
                    "failed_period": [chunk[0], chunk[-1]],
                    "synced_days": synced,
                    "attempted_days": attempted,
                    "rows_stored": rows_stored,
                    "db": str(path),
                }
            try:
                rows_stored += _store(
                    connection, source, resource, chunk, result["rows"], result["complete"]
                )
            except ValueError as exc:
                _mark_failed(connection, source, resource, chunk)
                return {
                    "ok": False,
                    "source": source,
                    "resource": resource,
                    "error": str(exc),
                    "failed_period": [chunk[0], chunk[-1]],
                    "synced_days": synced,
                    "attempted_days": attempted,
                    "rows_stored": rows_stored,
                    "db": str(path),
                }
            if result["complete"]:
                synced += len(chunk)
            if not result["complete"]:
                incomplete += chunk
    finally:
        connection.close()
    return {
        "ok": True,
        "source": source,
        "resource": resource,
        "period": [start, end],
        "requested_period": [start, end],
        "available_period": [start, min(end, available_end)] if available else None,
        "lagged_days": lagged,
        "already_synced_days": len(done) if not force else 0,
        "synced_days": synced,
        "attempted_days": attempted,
        "rows_stored": rows_stored,
        "incomplete_days": incomplete,
        "db": str(path),
    }


def status(path: Path) -> dict[str, Any]:
    """Summarize requested and retained calendar-day coverage without modifying the database."""
    connection = connect(path, create=False)
    try:
        connection.execute("BEGIN")
        ledger: dict[tuple[str, str], list[tuple[str, int, str, str | None, str | None]]] = {}
        for source, resource, day, rows, state, fetched_at, attempt in connection.execute(
            "SELECT source, resource, date, rows, state, fetched_at, last_attempt_state"
            " FROM sync_log"
            " ORDER BY source, resource, date"
        ):
            ledger.setdefault((source, resource), []).append(
                (day, rows, state, fetched_at, attempt)
            )
        entries = []
        for (source, resource), days in ledger.items():
            stored = connection.execute(
                f"SELECT COUNT(*) FROM {SOURCES[source].table} WHERE resource = ?", (resource,)
            ).fetchone()[0]
            dates = [day for day, *_ in days]
            coverage = {
                state: [day for day, _, current, _, _ in days if current == state]
                for state in ("complete", "empty", "partial", "failed", "pending", "lagged")
            }
            requested = set(dates)
            coverage["unrequested"] = [
                day for day in _days(dates[0], dates[-1]) if day not in requested
            ]
            entries.append(
                {
                    "source": source,
                    "resource": resource,
                    "schema_version": SCHEMA_VERSION,
                    "dimensions": list(SOURCES[source].dimensions),
                    "metrics": list(SOURCES[source].metrics),
                    "non_additive_metrics": list(SOURCES[source].non_additive),
                    "date_timezone": SOURCES[source].date_timezone,
                    "lag_cutoff_policy": (
                        "resource_pacific_date"
                        if SOURCES[source].date_timezone == "America/Los_Angeles"
                        else "conservative_utc_minus_one_day"
                    ),
                    "rows": stored,
                    "first_date": dates[0],
                    "last_date": dates[-1],
                    "requested_days": len(days),
                    "synced_days": len(coverage["complete"]) + len(coverage["empty"]),
                    "empty_days": len(coverage["empty"]),
                    "incomplete_days": coverage["partial"],
                    "coverage": coverage,
                    "gaps": _gaps(dates),
                    "last_fetched_at": max(
                        (fetched for _, _, _, fetched, _ in days if fetched), default=None
                    ),
                    "last_attempts": {
                        day: attempt
                        for day, _, _, _, attempt in days
                        if attempt and attempt not in ("complete", "empty")
                    },
                }
            )
    finally:
        connection.close()
    return {"ok": True, "db": str(path), "schema_version": SCHEMA_VERSION, "sources": entries}


def query(
    path: Path,
    source: str,
    *,
    resource: str | None = None,
    start_date: str | None = None,
    end_date: str | None = None,
    match: str | None = None,
    limit: int = 1000,
    out: str | None = None,
) -> dict[str, Any]:
    """Read bounded ordered rows with the coverage state of each retained day."""
    spec = _source(source)
    if not 1 <= limit <= MAX_EXPORT_ROWS:
        raise ValueError(f"limit must be between 1 and {MAX_EXPORT_ROWS}")
    for value in (start_date, end_date):
        if value:
            date.fromisoformat(value)
    if start_date and end_date and start_date > end_date:
        raise ValueError("start_date must not be after end_date")
    clauses, params = [], [source]
    if resource:
        clauses.append("facts.resource = ?")
        params.append(resource)
    if start_date:
        clauses.append("facts.date >= ?")
        params.append(start_date)
    if end_date:
        clauses.append("facts.date <= ?")
        params.append(end_date)
    if match:
        clauses.append("(" + " OR ".join(f"instr(facts.{d}, ?) > 0" for d in spec.dimensions) + ")")
        params += [match] * len(spec.dimensions)
    fact_columns = ["resource", "date", *spec.dimensions, *spec.metrics]
    columns = [*fact_columns, "coverage_state"]
    sql = (
        f"SELECT {', '.join('facts.' + column for column in fact_columns)},"
        f" sync_log.state FROM {spec.table} AS facts"
        " JOIN sync_log ON sync_log.source=? AND sync_log.resource=facts.resource"
        " AND sync_log.date=facts.date"
    )
    if clauses:
        sql += " WHERE " + " AND ".join(clauses)
    sql += " ORDER BY facts.resource, facts.date, " + ", ".join(
        "facts." + name for name in spec.dimensions
    )
    connection = connect(path, create=False)
    try:
        if out:
            target = Path(out)
            if target.exists():
                raise ValueError(f"refusing to overwrite existing file {target}")
            count = 0
            has_more = False
            created = False
            try:
                descriptor = os.open(target, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
                created = True
                with os.fdopen(descriptor, "w", newline="", encoding="utf-8") as handle:
                    writer = csv.writer(handle)
                    writer.writerow(columns)
                    cursor = connection.execute(sql + " LIMIT ?", [*params, limit + 1])
                    for row in cursor:
                        if count == limit:
                            has_more = True
                            break
                        writer.writerow(row)
                        count += 1
            except BaseException:
                if created:
                    target.unlink(missing_ok=True)
                raise
            return {
                "ok": True,
                "source": source,
                "out": str(target),
                "rows": count,
                "has_more": has_more,
                "non_additive_metrics": list(spec.non_additive),
            }
        cursor = connection.execute(sql + " LIMIT ?", [*params, limit + 1])
        rows = [dict(zip(columns, row, strict=True)) for row in cursor]
    finally:
        connection.close()
    return {
        "ok": True,
        "source": source,
        "columns": columns,
        "rows": rows[:limit],
        "returned": min(len(rows), limit),
        "has_more": len(rows) > limit,
        "non_additive_metrics": list(spec.non_additive),
        "date_timezone": spec.date_timezone,
        "lag_cutoff_policy": (
            "resource_pacific_date"
            if spec.date_timezone == "America/Los_Angeles"
            else "conservative_utc_minus_one_day"
        ),
    }

"""Private local URL Inspection history and per-property daily request budget."""

from __future__ import annotations

import json
import os
import stat
import sqlite3
import uuid
from contextlib import closing
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from seohead.core.sqlite import open_readonly, open_writer
from seohead.data_sources import credentials

FORMAT = "seohead.gsc-inspection-history.v1"
QUOTA_FORMAT = "seohead.gsc-inspection-quota.v2"
DB_NAME = "gsc-inspection.sqlite"
MAX_DAILY_CALLS = 2000
MAX_MINUTE_CALLS = 600
MAX_BATCH = 50
MAX_RECORDS = 100_000
MAX_RECORD_BYTES = 32_768
MAX_PAGE_ROWS = 250
MAX_PAGE_BYTES = 2 * 1024 * 1024
MAX_RESERVATION_URLS = 100_000
MAX_URL_BYTES = 8_192
MAX_RESERVATION_BYTES = 1024 * 1024
MAX_SETTLEMENT_BYTES = 2 * 1024 * 1024


def _path(project: str) -> Path:
    root = Path(project)
    if root.is_symlink() or not root.is_dir():
        raise ValueError("project must be a regular directory")
    manifest = root / "project.json"
    if manifest.is_symlink() or not manifest.is_file() or manifest.stat().st_nlink != 1:
        raise ValueError("project requires a regular, unaliased project.json")
    return root / DB_NAME


def _quota_path() -> Path:
    directory = credentials.CONFIG_ROOT / "seohead"
    if directory.is_symlink():
        raise ValueError("GSC inspection quota directory must not be a symlink")
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not directory.is_dir() or not credentials.is_private_mode(directory.stat().st_mode):
        raise ValueError("GSC inspection quota directory must be private")
    return directory / "gsc-inspection-quota.sqlite"


def _connect(path: Path, *, create: bool) -> sqlite3.Connection:
    if create and not os.path.lexists(path):
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        os.close(descriptor)
    info = path.lstat()
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or not credentials.is_private_mode(info.st_mode)
    ):
        raise ValueError("GSC inspection history must be a regular unaliased file")
    con = open_writer(path) if create else open_readonly(path.absolute())
    try:
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("BEGIN IMMEDIATE" if create else "BEGIN")
        tables = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if not tables and create and not info.st_size:
            con.execute("CREATE TABLE metadata(format TEXT PRIMARY KEY)")
            con.execute("INSERT INTO metadata VALUES (?)", (FORMAT,))
            con.execute(
                "CREATE TABLE inspections(id INTEGER PRIMARY KEY,site_url TEXT NOT NULL,url TEXT NOT NULL,"
                " inspected_at TEXT NOT NULL,state TEXT NOT NULL,index_status TEXT,amp_result TEXT,"
                " rich_results_result TEXT)"
            )
            con.execute("CREATE INDEX inspections_site_time ON inspections(site_url,inspected_at DESC,id DESC)")
        elif tables != {"metadata", "inspections"}:
            raise ValueError("existing database is not GSC inspection history")
        elif con.execute("SELECT format FROM metadata").fetchall() != [(FORMAT,)]:
            raise ValueError("unsupported GSC inspection history format")
        if not create:
            con.execute("PRAGMA query_only=ON")
        return con
    except BaseException:
        con.close()
        raise


def _connect_quota(path: Path) -> sqlite3.Connection:
    if not os.path.lexists(path):
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        os.close(descriptor)
    info = path.lstat()
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or not credentials.is_private_mode(info.st_mode)
    ):
        raise ValueError("GSC inspection quota must be a regular unaliased file")
    con = open_writer(path)
    try:
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("BEGIN IMMEDIATE")
        tables = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if not tables and not info.st_size:
            con.execute("CREATE TABLE metadata(format TEXT PRIMARY KEY)")
            con.execute("INSERT INTO metadata VALUES (?)", (QUOTA_FORMAT,))
            con.execute(
                "CREATE TABLE reservations(id TEXT PRIMARY KEY,site_url TEXT NOT NULL,"
                "reserved_at TEXT NOT NULL,count INTEGER NOT NULL,state TEXT NOT NULL,"
                "urls TEXT NOT NULL DEFAULT '[]')"
            )
            con.execute("CREATE INDEX reservations_site_time ON reservations(site_url,reserved_at)")
        elif tables != {"metadata", "reservations"}:
            raise ValueError("existing database is not a GSC inspection quota ledger")
        else:
            format_row = con.execute("SELECT format FROM metadata").fetchall()
            columns = {row[1] for row in con.execute("PRAGMA table_info(reservations)")}
            if format_row == [("seohead.gsc-inspection-quota.v1",)]:
                if "urls" not in columns:
                    con.execute("ALTER TABLE reservations ADD COLUMN urls TEXT NOT NULL DEFAULT '[]'")
                con.execute("UPDATE metadata SET format=?", (QUOTA_FORMAT,))
            elif format_row != [(QUOTA_FORMAT,)]:
                raise ValueError("unsupported GSC inspection quota format")
            elif "urls" not in columns:
                raise ValueError("GSC quota ledger is missing its URL population binding")
        return con
    except BaseException:
        con.close()
        raise


def reserve(project: str, site_url: str, urls: list[str], *, now: datetime | None = None) -> dict[str, Any]:
    """Reserve a capped subset before network access so interrupted runs still count toward budget."""
    if (
        not isinstance(site_url, str)
        or not site_url
        or len(site_url.encode("utf-8")) > 2048
        or not isinstance(urls, list)
    ):
        raise ValueError("site_url and urls are required")
    if len(urls) > MAX_RESERVATION_URLS or not all(
        isinstance(url, str) and url and len(url.encode("utf-8")) <= MAX_URL_BYTES
        for url in urls
    ):
        raise ValueError("inspection URLs must be a bounded nonempty string list")
    unique = list(dict.fromkeys(urls))
    if len(json.dumps(unique, ensure_ascii=False).encode("utf-8")) > MAX_RESERVATION_BYTES:
        raise ValueError("inspection URL population exceeds the 1 MiB reservation limit")
    current = now or datetime.now(timezone.utc)
    if current.tzinfo is None:
        raise ValueError("quota clock must include a timezone")
    current = current.astimezone(timezone.utc)
    day = current.date().isoformat()
    minute = current.strftime("%Y-%m-%dT%H:%M")
    reserved_at = current.isoformat(timespec="seconds").replace("+00:00", "Z")
    cutoff_day = (current - timedelta(hours=24)).isoformat(timespec="seconds").replace("+00:00", "Z")
    cutoff_minute = (current - timedelta(minutes=1)).isoformat(timespec="seconds").replace("+00:00", "Z")
    path = _quota_path()
    reservation_id = str(uuid.uuid4())
    with closing(_connect_quota(path)) as con, con:
        retention_cutoff = (current - timedelta(days=30)).isoformat(timespec="seconds").replace("+00:00", "Z")
        con.execute("DELETE FROM reservations WHERE reserved_at<?", (retention_cutoff,))
        used = con.execute(
            "SELECT COALESCE(SUM(count),0) FROM reservations"
            " WHERE site_url=? AND state!='released' AND reserved_at>=?",
            (site_url, cutoff_day),
        ).fetchone()[0]
        minute_used = con.execute(
            "SELECT COALESCE(SUM(count),0) FROM reservations"
            " WHERE site_url=? AND state!='released' AND reserved_at>=?",
            (site_url, cutoff_minute),
        ).fetchone()[0]
        allowed = min(
            len(unique),
            MAX_BATCH,
            max(0, MAX_DAILY_CALLS - used),
            max(0, MAX_MINUTE_CALLS - minute_used),
        )
        selected = unique[:allowed]
        if selected:
            con.execute(
                "INSERT INTO reservations(id,site_url,reserved_at,count,state,urls)"
                " VALUES(?,?,?,?, 'reserved',?)",
                (
                    reservation_id,
                    site_url,
                    reserved_at,
                    len(selected),
                    json.dumps(selected, ensure_ascii=False, separators=(",", ":")),
                ),
            )
    return {
        "reservation_id": reservation_id if selected else None,
        "site_url": site_url,
        "day": day,
        "urls": selected,
        "requested": len(unique),
        "reserved": len(selected),
        "used_before": used,
        "remaining_after": MAX_DAILY_CALLS - used - len(selected),
        "minute_remaining_after": MAX_MINUTE_CALLS - minute_used - len(selected),
        "deferred": len(unique) - len(selected),
        "budget_window": "rolling 24 hours and rolling minute",
        "shared_provider_usage": "unknown; this ledger counts only calls through this machine",
    }


def settle(
    project: str,
    reservation_id: str,
    outcomes: list[dict[str, Any]],
    *,
    requests_made: int,
    now: datetime | None = None,
) -> dict[str, Any]:
    if type(requests_made) is not int or requests_made < 0 or not isinstance(outcomes, list):
        raise ValueError("invalid inspection settlement")
    quota_path = _quota_path()
    timestamp = (now or datetime.now(timezone.utc)).isoformat().replace("+00:00", "Z")
    stored = 0
    with closing(_connect_quota(quota_path)) as quota_con, quota_con:
        row = quota_con.execute(
            "SELECT site_url,reserved_at,count,state,urls FROM reservations WHERE id=?",
            (reservation_id,),
        ).fetchone()
        if row is None or row[3] != "reserved" or requests_made > row[2]:
            raise ValueError("unknown or already settled inspection reservation")
        site_url, _reserved_at, reserved, _state, reserved_urls_json = row
        if requests_made == 0:
            if outcomes:
                raise ValueError("zero-request settlement cannot contain inspection outcomes")
            quota_con.execute("UPDATE reservations SET state='released' WHERE id=?", (reservation_id,))
            return {"state": "released", "stored": 0}
        if len(outcomes) > requests_made:
            raise ValueError("provider returned more inspection rows than requests")
        try:
            reserved_urls = json.loads(reserved_urls_json)
        except (TypeError, ValueError):
            reserved_urls = None
        if not isinstance(reserved_urls, list) or not all(
            isinstance(url, str) and url for url in reserved_urls
        ):
            quota_con.execute(
                "UPDATE reservations SET state='unbound_legacy' WHERE id=?", (reservation_id,)
            )
            return {"state": "unbound_reservation", "stored": 0}
        allowed_urls = set(reserved_urls)
        seen_urls: set[str] = set()
        payload_rows = []
        total_payload_bytes = 0
        for item in outcomes:
            if not isinstance(item, dict):
                raise ValueError("inspection outcome must be an object")
            url = item.get("url")
            if (
                not isinstance(url, str)
                or not url
                or len(url.encode("utf-8")) > MAX_URL_BYTES
                or url not in allowed_urls
                or url in seen_urls
            ):
                raise ValueError("inspection outcome URL is outside the reserved population")
            seen_urls.add(url)
            total_payload_bytes += len(url.encode("utf-8"))
            if total_payload_bytes > MAX_SETTLEMENT_BYTES:
                raise ValueError("inspection settlement exceeds its byte limit")
            if item.get("state") not in {"complete", "failed"}:
                raise ValueError("inspection outcome state is invalid")
            fields = []
            for key in ("index_status", "amp_result", "rich_results_result"):
                value = item.get(key) or {}
                if not isinstance(value, dict):
                    raise ValueError("inspection result sections must be objects")
                encoded = json.dumps(
                    value,
                    ensure_ascii=False,
                    sort_keys=True,
                    separators=(",", ":"),
                    allow_nan=False,
                )
                size = len(encoded.encode("utf-8"))
                if size > MAX_RECORD_BYTES:
                    raise ValueError("inspection result section exceeds its byte limit")
                total_payload_bytes += size
                if total_payload_bytes > MAX_SETTLEMENT_BYTES:
                    raise ValueError("inspection settlement exceeds its byte limit")
                fields.append(encoded)
            payload_rows.append((url, item["state"], *fields))
        history_path = _path(project)
        with closing(_connect(history_path, create=True)) as history_con, history_con:
            total = history_con.execute("SELECT COUNT(*) FROM inspections").fetchone()[0]
            if total + len(payload_rows) > MAX_RECORDS:
                quota_con.execute(
                    "UPDATE reservations SET state='retained_unstored' WHERE id=?",
                    (reservation_id,),
                )
                return {"state": "history_full", "stored": 0}
            for url, state, *payloads in payload_rows:
                history_con.execute(
                    "INSERT INTO inspections(site_url,url,inspected_at,state,index_status,amp_result,rich_results_result)"
                    " VALUES(?,?,?,?,?,?,?)",
                    (site_url, url, timestamp, state, *payloads),
                )
                stored += 1
        quota_con.execute(
            "UPDATE reservations SET state='complete' WHERE id=?", (reservation_id,)
        )
    return {"state": "stored", "stored": stored}


def latest(
    project: str,
    site_url: str,
    *,
    limit: int = 100,
    offset: int = 0,
    since: str | None = None,
) -> dict[str, Any]:
    if type(limit) is not int or not 1 <= limit <= MAX_PAGE_ROWS:
        raise ValueError(f"limit must be 1..{MAX_PAGE_ROWS}")
    if type(offset) is not int or offset < 0:
        raise ValueError("offset must be a nonnegative integer")
    if not isinstance(site_url, str) or not site_url or len(site_url.encode("utf-8")) > 2048:
        raise ValueError("site_url must be a bounded nonempty string")
    path = _path(project)
    selected: dict[str, dict[str, Any]] = {}
    page_bytes = 0
    has_more = False
    with closing(_connect(path, create=False)) as con:
        where = "site_url=?"
        params: list[Any] = [site_url]
        if since:
            datetime.fromisoformat(since.replace("Z", "+00:00"))
            where += " AND inspected_at>=?"
            params.append(since)
        total = con.execute(
            "SELECT COUNT(DISTINCT url) FROM inspections WHERE " + where, params
        ).fetchone()[0]
        cursor = con.execute(
            "SELECT url,inspected_at,state,index_status,amp_result,rich_results_result FROM ("
            " SELECT url,inspected_at,state,index_status,amp_result,rich_results_result,id,"
            " ROW_NUMBER() OVER(PARTITION BY url ORDER BY inspected_at DESC,id DESC) AS rank"
            f" FROM inspections WHERE {where}) WHERE rank=1"
            " ORDER BY inspected_at DESC,url LIMIT ? OFFSET ?",
            [*params, limit, offset],
        )
        for url, inspected_at, state, index_status, amp, rich in cursor:
            row = {
                "url": url,
                "inspected_at": inspected_at,
                "state": state,
                "index_status": json.loads(index_status or "{}"),
                "amp_result": json.loads(amp or "{}"),
                "rich_results_result": json.loads(rich or "{}"),
            }
            size = len(json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
            if page_bytes + size > MAX_PAGE_BYTES - 4096:
                has_more = True
                break
            selected[url] = row
            page_bytes += size
    has_more = has_more or offset + len(selected) < total
    return {
        "schema_version": FORMAT,
        "site_url": site_url,
        "total": total,
        "offset": offset,
        "limit": limit,
        "has_more": has_more,
        "next_offset": offset + len(selected) if has_more else None,
        "truncated_by_byte_limit": page_bytes >= MAX_PAGE_BYTES or (has_more and len(selected) < limit),
        "rows": list(selected.values()),
    }

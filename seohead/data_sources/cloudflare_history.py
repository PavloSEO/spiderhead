"""Append-only local history for bounded Cloudflare aggregate URL evidence."""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import sqlite3
import stat
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.core.sqlite import open_readonly, open_writer
from seohead.data_sources.credentials import is_private_mode

FORMAT = "seohead.cloudflare-history.v1"
DB_NAME = "cloudflare.sqlite"
MAX_SNAPSHOT_BYTES = 32 * 1024 * 1024
MAX_SNAPSHOTS = 1_000
MAX_PAGE_BYTES = 2 * 1024 * 1024
MAX_FACT_PAGE = 500
MAX_ROWS_WITH_FACTS = 8


def _path(project: str) -> Path:
    root = Path(project)
    if root.is_symlink() or not root.is_dir():
        raise ValueError("project must be a regular directory")
    manifest = root / "project.json"
    if manifest.is_symlink() or not manifest.is_file() or manifest.stat().st_nlink != 1:
        raise ValueError("project requires a regular, unaliased project.json")
    return root / DB_NAME


def _connect(path: Path, *, create: bool) -> sqlite3.Connection:
    if create and not os.path.lexists(path):
        try:
            descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        except FileExistsError:
            pass
        else:
            os.close(descriptor)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or not is_private_mode(info.st_mode):
        raise ValueError("Cloudflare history must be a private regular unaliased file")
    con = open_writer(path) if create else open_readonly(path.absolute())
    try:
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("BEGIN IMMEDIATE" if create else "BEGIN")
        tables = {
            row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if not tables and create and not info.st_size:
            con.execute("CREATE TABLE cloudflare_meta(format TEXT PRIMARY KEY)")
            con.execute("INSERT INTO cloudflare_meta VALUES (?)", (FORMAT,))
            con.execute(
                "CREATE TABLE cloudflare_snapshots (id INTEGER PRIMARY KEY, zone_id TEXT NOT NULL,"
                " zone TEXT NOT NULL, observed_at TEXT NOT NULL, fingerprint TEXT NOT NULL,"
                " period_from TEXT NOT NULL, period_to TEXT NOT NULL, state TEXT NOT NULL,"
                " sampled INTEGER NOT NULL, truncated_days TEXT NOT NULL, payload TEXT NOT NULL,"
                " UNIQUE(zone_id, observed_at, fingerprint))"
            )
            con.execute(
                "CREATE INDEX cloudflare_snapshots_zone_period"
                " ON cloudflare_snapshots(zone_id, period_from, period_to, id)"
            )
        elif tables != {"cloudflare_meta", "cloudflare_snapshots"}:
            raise ValueError("existing database is not a Cloudflare history database")
        elif con.execute("SELECT format FROM cloudflare_meta").fetchall() != [(FORMAT,)]:
            raise ValueError("unsupported Cloudflare history format")
        if not create:
            con.execute("PRAGMA query_only=ON")
        return con
    except BaseException:
        con.close()
        raise


def save_snapshot(
    result: dict[str, Any], *, project: str, observed_at: str | None = None
) -> dict[str, Any]:
    """Append one aggregate snapshot without retaining IPs or changing older snapshots."""
    if not result.get("ok") or not result.get("zone_id"):
        raise ValueError("a successful Cloudflare result with zone ID is required")
    payload = {
        "url_facts": result.get("url_facts") or [],
        "url_facts_truncated": bool(result.get("url_facts_truncated")),
        "url_facts_omitted": result.get("url_facts_omitted", 0),
        "requests": result.get("requests"),
        "sampled": result.get("sampled"),
        "verification": result.get("verification"),
        "note": result.get("note"),
    }
    encoded = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    if len(encoded.encode("utf-8")) > MAX_SNAPSHOT_BYTES:
        raise ValueError("Cloudflare history snapshot exceeds 32 MiB")
    truncated_days = result.get("truncated_days") or []
    state = (
        "partial"
        if result.get("sampled") or truncated_days or payload["url_facts_truncated"]
        else "complete"
    )
    stamp = observed_at or datetime.now(timezone.utc).isoformat(timespec="seconds")
    fingerprint = hashlib.sha256(encoded.encode("utf-8")).hexdigest()
    path = _path(project)
    with contextlib.closing(_connect(path, create=True)) as con, con:
        count = con.execute("SELECT COUNT(*) FROM cloudflare_snapshots").fetchone()[0]
        prior = con.execute(
            "SELECT id FROM cloudflare_snapshots WHERE zone_id=? AND observed_at=? AND fingerprint=?",
            (result["zone_id"], stamp, fingerprint),
        ).fetchone()
        if prior:
            snapshot_id = prior[0]
        else:
            if count >= MAX_SNAPSHOTS:
                raise ValueError("Cloudflare history reached the 1000-snapshot limit")
            con.execute(
                "INSERT INTO cloudflare_snapshots"
                " (zone_id,zone,observed_at,fingerprint,period_from,period_to,state,sampled,truncated_days,payload)"
                " VALUES(?,?,?,?,?,?,?,?,?,?)",
                (
                    result["zone_id"],
                    result["zone"],
                    stamp,
                    fingerprint,
                    result["period"]["from"],
                    result["period"]["to"],
                    state,
                    int(bool(result.get("sampled"))),
                    json.dumps(truncated_days),
                    encoded,
                ),
            )
            snapshot_id = con.execute(
                "SELECT id FROM cloudflare_snapshots WHERE zone_id=? AND observed_at=? AND fingerprint=?",
                (result["zone_id"], stamp, fingerprint),
            ).fetchone()[0]
    return {"state": "stored", "snapshot_id": snapshot_id, "snapshot_state": state}


def list_snapshots(
    *,
    project: str,
    zone_id: str | None = None,
    snapshot_id: int | None = None,
    limit: int = 100,
    offset: int = 0,
    include_facts: bool = False,
    facts_offset: int = 0,
    facts_limit: int = 100,
) -> dict[str, Any]:
    """Read a bounded page of snapshots from the project-local aggregate history."""
    if type(limit) is not int or not 1 <= limit <= 100 or type(offset) is not int or offset < 0:
        raise ValueError("limit must be 1..100 and offset must be nonnegative")
    if type(include_facts) is not bool:
        raise ValueError("include_facts must be a boolean")
    if type(facts_offset) is not int or facts_offset < 0:
        raise ValueError("facts_offset must be nonnegative")
    if type(facts_limit) is not int or not 1 <= facts_limit <= MAX_FACT_PAGE:
        raise ValueError(f"facts_limit must be 1..{MAX_FACT_PAGE}")
    if snapshot_id is not None and (type(snapshot_id) is not int or snapshot_id < 1):
        raise ValueError("snapshot_id must be positive")
    if include_facts and limit > MAX_ROWS_WITH_FACTS:
        limit = MAX_ROWS_WITH_FACTS
    path = _path(project)
    result_rows = []
    page_bytes = 0
    byte_limited = False
    with contextlib.closing(_connect(path, create=False)) as con:
        filters = []
        params: list[Any] = []
        if zone_id:
            filters.append("zone_id=?")
            params.append(zone_id)
        if snapshot_id is not None:
            filters.append("id=?")
            params.append(snapshot_id)
        clause = " WHERE " + " AND ".join(filters) if filters else ""
        total = con.execute(
            f"SELECT COUNT(*) FROM cloudflare_snapshots{clause}", params
        ).fetchone()[0]
        payload_column = ",payload" if include_facts else ",length(payload)"
        query = (
            "SELECT id,zone_id,zone,observed_at,period_from,period_to,state,sampled,truncated_days"
            + payload_column
            + f" FROM cloudflare_snapshots{clause} ORDER BY id DESC LIMIT ? OFFSET ?"
        )
        cursor = con.execute(
            query,
            [*params, limit, offset],
        )
        for row in cursor:
            payload = json.loads(row[9]) if include_facts else None
            all_facts = payload.get("url_facts") if isinstance(payload, dict) else None
            if not isinstance(all_facts, list):
                all_facts = []
            stored_omitted = (
                payload.get("url_facts_omitted", 0) if isinstance(payload, dict) else None
            )
            stored_omitted = (
                stored_omitted if type(stored_omitted) is int and stored_omitted >= 0 else None
            )
            facts = []
            if include_facts:
                for item in all_facts[facts_offset : facts_offset + facts_limit]:
                    if not isinstance(item, dict):
                        continue
                    facts.append(
                        {
                            "host": str(item.get("host") or "unknown")[:255],
                            "path": str(item.get("path") or "/")[:2048],
                            "date": str(item.get("date") or "")[:10],
                            "bot_family": str(item.get("bot_family") or "unknown")[:64],
                            "bot_name": str(item.get("bot_name") or "unknown")[:128],
                            "verified_bot_category": (
                                str(item["verified_bot_category"])[:128]
                                if isinstance(item.get("verified_bot_category"), str)
                                else None
                            ),
                            "edge_status": item.get("edge_status")
                            if type(item.get("edge_status")) is int
                            else None,
                            "origin_status": item.get("origin_status")
                            if type(item.get("origin_status")) is int
                            else None,
                            "cache_status": str(item.get("cache_status") or "unknown")[:64],
                            "edge_redirect": bool(item.get("edge_redirect")),
                            "requests": item.get("requests")
                            if type(item.get("requests")) is int and item["requests"] >= 0
                            else None,
                        }
                    )
            entry = {
                "snapshot_id": row[0],
                "zone_id": row[1],
                "zone": row[2],
                "observed_at": row[3],
                "period": {"from": row[4], "to": row[5]},
                "state": row[6],
                "sampled": bool(row[7]),
                "truncated_days": json.loads(row[8]),
                "url_facts_available": True,
            }
            if include_facts:
                entry.update(
                    {
                        "url_facts": facts,
                        "url_facts_source_truncated": bool(payload.get("url_facts_truncated")),
                        "url_facts_truncated": bool(payload.get("url_facts_truncated"))
                        or bool(stored_omitted),
                        "url_facts_omitted": stored_omitted,
                        "url_facts_total_retained": len(all_facts),
                        "url_facts_offset": facts_offset,
                        "url_facts_next_offset": (
                            facts_offset + len(facts)
                            if facts_offset + len(facts) < len(all_facts)
                            else None
                        ),
                        "url_facts_page_omitted": max(
                            0, len(all_facts) - facts_offset - len(facts)
                        ),
                    }
                )
            else:
                entry["url_facts_payload_bytes"] = row[9]
            size = len(json.dumps(entry, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
            if page_bytes + size > MAX_PAGE_BYTES - 4096:
                byte_limited = True
                break
            result_rows.append(entry)
            page_bytes += size
    has_more = offset + len(result_rows) < total
    return {
        "ok": True,
        "state": "partial" if has_more else "complete",
        "source": "cloudflare-history",
        "schema_version": FORMAT,
        "total": total,
        "offset": offset,
        "limit": len(result_rows),
        "has_more": has_more,
        "next_offset": offset + len(result_rows) if has_more else None,
        "truncated_by_byte_limit": byte_limited,
        "facts_included": include_facts,
        "facts_page": {"offset": facts_offset, "limit": facts_limit},
        "rows": result_rows,
    }


def list_zones(*, project: str, limit: int = 100, offset: int = 0) -> dict[str, Any]:
    """List distinct locally retained Cloudflare zone IDs without network access."""
    if type(limit) is not int or not 1 <= limit <= 100 or type(offset) is not int or offset < 0:
        raise ValueError("limit must be 1..100 and offset must be nonnegative")
    path = _path(project)
    with contextlib.closing(_connect(path, create=False)) as con:
        total = con.execute("SELECT COUNT(DISTINCT zone_id) FROM cloudflare_snapshots").fetchone()[
            0
        ]
        zones = con.execute(
            "SELECT zone_id FROM cloudflare_snapshots GROUP BY zone_id ORDER BY MAX(id) DESC LIMIT ? OFFSET ?",
            (limit, offset),
        ).fetchall()
        result = []
        for (zone_id,) in zones:
            row = con.execute(
                "SELECT zone,observed_at,period_from,period_to,state,sampled,truncated_days"
                ",payload"
                " FROM cloudflare_snapshots WHERE zone_id=? ORDER BY id DESC LIMIT 1",
                (zone_id,),
            ).fetchone()
            result.append(
                {
                    "zone_id": zone_id,
                    "zone": row[0],
                    "latest_observed_at": row[1],
                    "latest_period": {"from": row[2], "to": row[3]},
                    "state": row[4],
                    "sampled": bool(row[5]),
                    "truncated": bool(json.loads(row[6]))
                    or bool(json.loads(row[7]).get("url_facts_truncated")),
                }
            )
    return {
        "schema_version": FORMAT,
        "total": total,
        "offset": offset,
        "limit": limit,
        "zones": result,
    }

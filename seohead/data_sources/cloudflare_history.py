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
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or not is_private_mode(info.st_mode)
    ):
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
    state = "partial" if result.get("sampled") or truncated_days or payload["url_facts_truncated"] else "complete"
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
    *, project: str, zone_id: str | None = None, limit: int = 100, offset: int = 0
) -> dict[str, Any]:
    """Read a bounded page of snapshots from the project-local aggregate history."""
    if type(limit) is not int or not 1 <= limit <= 100 or type(offset) is not int or offset < 0:
        raise ValueError("limit must be 1..100 and offset must be nonnegative")
    path = _path(project)
    with contextlib.closing(_connect(path, create=False)) as con:
        clause, params = (" WHERE zone_id=?", [zone_id]) if zone_id else ("", [])
        total = con.execute(f"SELECT COUNT(*) FROM cloudflare_snapshots{clause}", params).fetchone()[0]
        rows = con.execute(
            "SELECT id,zone_id,zone,observed_at,period_from,period_to,state,sampled,truncated_days,payload"
            f" FROM cloudflare_snapshots{clause} ORDER BY id DESC LIMIT ? OFFSET ?",
            [*params, limit, offset],
        ).fetchall()
    return {
        "ok": True,
        "state": "complete",
        "source": "cloudflare-history",
        "schema_version": FORMAT,
        "total": total,
        "offset": offset,
        "limit": limit,
        "rows": [
            {
                "snapshot_id": row[0],
                "zone_id": row[1],
                "zone": row[2],
                "observed_at": row[3],
                "period": {"from": row[4], "to": row[5]},
                "state": row[6],
                "sampled": bool(row[7]),
                "truncated_days": json.loads(row[8]),
                **json.loads(row[9]),
            }
            for row in rows
        ],
    }


def list_zones(*, project: str, limit: int = 100, offset: int = 0) -> dict[str, Any]:
    """List distinct locally retained Cloudflare zone IDs without network access."""
    if type(limit) is not int or not 1 <= limit <= 100 or type(offset) is not int or offset < 0:
        raise ValueError("limit must be 1..100 and offset must be nonnegative")
    path = _path(project)
    with contextlib.closing(_connect(path, create=False)) as con:
        total = con.execute("SELECT COUNT(DISTINCT zone_id) FROM cloudflare_snapshots").fetchone()[0]
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
                    "truncated": bool(json.loads(row[6])) or bool(
                        json.loads(row[7]).get("url_facts_truncated")
                    ),
                }
            )
    return {"schema_version": FORMAT, "total": total, "offset": offset, "limit": limit, "zones": result}

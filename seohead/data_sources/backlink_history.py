"""Versioned, local backlink snapshots isolated from scan.v1 and provider time series."""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import sqlite3
import stat
from pathlib import Path

from seohead.core.sqlite import open_readonly, open_writer

FORMAT = "seohead.backlink-history.v1"
DB_NAME = "backlinks.sqlite"
MAX_SNAPSHOT_BYTES = 64 * 1024 * 1024


def _path(db: str | None, project: str | None) -> Path:
    if bool(db) == bool(project):
        raise ValueError("pass exactly one of db or project")
    if project:
        root = Path(project)
        if not (root / "project.json").is_file():
            raise ValueError("project requires project.json")
        return root / DB_NAME
    return Path(str(db))


def _connect(path: Path, *, create: bool) -> sqlite3.Connection:
    if create and not os.path.lexists(path):
        try:
            descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        except FileExistsError:
            pass
        else:
            os.close(descriptor)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError("backlink DB must be a regular unaliased file")
    con = open_writer(path) if create else open_readonly(path.absolute())
    try:
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("BEGIN IMMEDIATE" if create else "BEGIN")
        tables = {
            row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if not tables and create and not info.st_size:
            con.execute("CREATE TABLE backlink_meta(format TEXT PRIMARY KEY)")
            con.execute("INSERT INTO backlink_meta VALUES (?)", (FORMAT,))
            con.execute(
                "CREATE TABLE backlink_snapshots (id INTEGER PRIMARY KEY, domain TEXT NOT NULL,"
                " observed_at TEXT NOT NULL, fingerprint TEXT NOT NULL, payload TEXT NOT NULL,"
                " UNIQUE(domain, observed_at, fingerprint))"
            )
            con.execute("CREATE INDEX backlink_snapshots_domain ON backlink_snapshots(domain,id)")
        elif tables != {"backlink_meta", "backlink_snapshots"}:
            raise ValueError("existing database is not a backlink history database")
        else:
            marker = con.execute("SELECT format FROM backlink_meta").fetchall()
            if marker != [(FORMAT,)]:
                raise ValueError("unsupported backlink history format")
        if not create:
            con.execute("PRAGMA query_only=ON")
        return con
    except BaseException:
        con.close()
        raise


def save_snapshot(
    domain: str,
    rows: list[dict],
    coverage: list[dict],
    domain_edges: list[dict],
    *,
    db: str | None = None,
    project: str | None = None,
    observed_at: str,
) -> dict:
    """Append atomically; retries are idempotent and old sampled absences remain unobserved."""
    path = _path(db, project)
    payload = json.dumps(
        {"rows": rows, "coverage": coverage, "domain_edges": domain_edges},
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    )
    encoded = payload.encode("utf-8")
    if len(encoded) > MAX_SNAPSHOT_BYTES:
        raise ValueError("backlink snapshot exceeds 64 MiB")
    fingerprint = hashlib.sha256(encoded).hexdigest()
    with contextlib.closing(_connect(path, create=True)) as con, con:
        prior = con.execute(
            "SELECT id, payload FROM backlink_snapshots WHERE domain=? ORDER BY id DESC LIMIT 1",
            (domain,),
        ).fetchone()
        con.execute(
            "INSERT OR IGNORE INTO backlink_snapshots(domain,observed_at,fingerprint,payload)"
            " VALUES(?,?,?,?)",
            (domain, observed_at, fingerprint, payload),
        )
        identifier = con.execute(
            "SELECT id FROM backlink_snapshots WHERE domain=? AND observed_at=? AND fingerprint=?",
            (domain, observed_at, fingerprint),
        ).fetchone()[0]
    before = (
        {(row["source_url"], row["target_url"]) for row in json.loads(prior[1])["rows"]}
        if prior
        else set()
    )
    after = {(row["source_url"], row["target_url"]) for row in rows}
    return {
        "db": str(path),
        "snapshot_id": identifier,
        "fingerprint": fingerprint,
        "previous_snapshot_id": prior[0] if prior and prior[0] != identifier else None,
        "newly_observed": len(after - before),
        "unobserved_since_previous": len(before - after),
        "confirmed_lost": sum(row["state"] == "lost" for row in rows),
        "absence_is_loss": False,
    }


def snapshot_history(
    *,
    db: str | None = None,
    project: str | None = None,
    domain: str | None = None,
    limit: int = 100,
    offset: int = 0,
) -> dict:
    """Read history without creating a DB or modifying a project workflow."""
    if type(limit) is not int or not 1 <= limit <= 1000 or type(offset) is not int or offset < 0:
        raise ValueError("limit must be 1..1000 and offset must be nonnegative")
    path = _path(db, project)
    with contextlib.closing(_connect(path, create=False)) as con:
        clause, params = (" WHERE domain=?", [domain]) if domain else ("", [])
        total = con.execute("SELECT COUNT(*) FROM backlink_snapshots" + clause, params).fetchone()[
            0
        ]
        rows = con.execute(
            "SELECT id,domain,observed_at,fingerprint FROM backlink_snapshots"
            + clause
            + " ORDER BY id DESC LIMIT ? OFFSET ?",
            [*params, limit, offset],
        ).fetchall()
    return {
        "ok": True,
        "db": str(path),
        "total": total,
        "snapshots": [
            dict(zip(("id", "domain", "observed_at", "fingerprint"), row, strict=True))
            for row in rows
        ],
        "has_more": offset + limit < total,
    }


def read_snapshot(snapshot_id: int, *, db: str | None = None, project: str | None = None) -> dict:
    if type(snapshot_id) is not int or snapshot_id < 1:
        raise ValueError("snapshot_id must be a positive integer")
    path = _path(db, project)
    with contextlib.closing(_connect(path, create=False)) as con:
        row = con.execute(
            "SELECT domain,observed_at,payload FROM backlink_snapshots WHERE id=?",
            (snapshot_id,),
        ).fetchone()
    if not row:
        raise ValueError("backlink snapshot was not found")
    return {
        "ok": True,
        "snapshot_id": snapshot_id,
        "domain": row[0],
        "observed_at": row[1],
        **json.loads(row[2]),
    }

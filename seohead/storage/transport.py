"""Optional scan.v2 response transport facts; unknown phases stay explicitly unknown."""

from __future__ import annotations

import sqlite3
from typing import Any

from . import ScanError


def ensure_schema(con: sqlite3.Connection) -> None:
    if con.execute("PRAGMA user_version").fetchone()[0] == 2:
        con.execute(
            "CREATE TABLE IF NOT EXISTS response_transport_meta(response_id INTEGER PRIMARY KEY,"
            "protocol TEXT,protocol_state TEXT NOT NULL,total_seconds REAL,"
            "dns_seconds REAL,dns_state TEXT NOT NULL DEFAULT 'unavailable',dns_reason TEXT NOT NULL DEFAULT 'not captured',"
            "connect_seconds REAL,connect_state TEXT NOT NULL DEFAULT 'unavailable',connect_reason TEXT NOT NULL DEFAULT 'not captured',"
            "tls_seconds REAL,tls_state TEXT NOT NULL DEFAULT 'unavailable',tls_reason TEXT NOT NULL DEFAULT 'not captured',"
            "ttfb_seconds REAL,ttfb_state TEXT NOT NULL DEFAULT 'unavailable',ttfb_reason TEXT NOT NULL DEFAULT 'not captured',"
            "download_seconds REAL,download_state TEXT NOT NULL DEFAULT 'unavailable',download_reason TEXT NOT NULL DEFAULT 'not captured')"
        )
        existing = {row[1] for row in con.execute("PRAGMA table_info(response_transport_meta)")}
        for phase in ("dns", "connect", "tls", "ttfb", "download"):
            for suffix, definition in (
                ("seconds", "REAL"),
                ("reason", "TEXT NOT NULL DEFAULT 'not captured'"),
            ):
                name = f"{phase}_{suffix}"
                if name not in existing:
                    con.execute(f"ALTER TABLE response_transport_meta ADD COLUMN {name} {definition}")
            state = f"{phase}_state"
            if state not in existing:
                con.execute(
                    f"ALTER TABLE response_transport_meta ADD COLUMN {state} "
                    "TEXT NOT NULL DEFAULT 'unavailable'"
                )


def record(con: sqlite3.Connection, response_id: int, event: Any) -> None:
    if con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return
    ensure_schema(con)
    protocol = event.http_version
    if protocol is not None and (not isinstance(protocol, str) or len(protocol) > 32):
        raise ScanError("captured HTTP protocol is invalid")
    if event.timing_state not in {"partial", "unavailable"}:
        raise ScanError("captured timing state is invalid")
    from seohead.crawl.capture import TRANSPORT_PHASES, unavailable_transport_phases

    phases = event.transport_phases or unavailable_transport_phases(
        "transport trace was unavailable"
    )
    if set(phases) != set(TRANSPORT_PHASES):
        raise ScanError("captured transport phases are invalid")
    values = []
    for name in TRANSPORT_PHASES:
        phase = phases[name]
        values.extend((phase["seconds"], phase["state"], phase["reason"]))
    con.execute(
        "INSERT INTO response_transport_meta(response_id,protocol,protocol_state,total_seconds,"
        "dns_seconds,dns_state,dns_reason,connect_seconds,connect_state,connect_reason,"
        "tls_seconds,tls_state,tls_reason,ttfb_seconds,ttfb_state,ttfb_reason,"
        "download_seconds,download_state,download_reason) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            response_id,
            protocol,
            "known" if protocol else "unavailable",
            event.response_time,
            *values,
        ),
    )

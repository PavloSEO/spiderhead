"""Read-only operational status summaries for validated scan artifacts."""

from __future__ import annotations

import sqlite3
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from . import ScanError, open_scan_mode
from .link_positions import position_summary


def scan_status(input_path: str, *, full_validation: bool = False) -> dict[str, Any]:
    """Summarize one scan snapshot without fetching, retrying, or changing its bytes.

    By default the scan is accepted by a light header/schema check and the response says
    ``validation: "light"``; ``full_validation=True`` runs the complete artifact validation
    (``"full"``). Counters come from SQL either way.
    """
    if not isinstance(input_path, str) or not input_path:
        raise ValueError("input_path is required")
    if type(full_validation) is not bool:
        raise ValueError("full_validation must be a boolean")
    path = Path(input_path)
    checked_at = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    try:
        con, validation = open_scan_mode(path, require_audit=False, light=not full_validation)
    except (OSError, sqlite3.Error, ScanError, ValueError, TypeError) as exc:
        # A status read distinguishes unavailable/damaged storage from an empty scan.
        scan_uuid = None
        checkpoint_available = None
        try:
            uri = path.resolve().as_uri() + "?mode=ro"
            with sqlite3.connect(uri, uri=True) as damaged:
                row = damaged.execute("SELECT scan_uuid FROM scan WHERE singleton=1").fetchone()
                scan_uuid = row[0] if row and isinstance(row[0], str) else None
                checkpoint_available = bool(
                    damaged.execute("SELECT 1 FROM resume_state WHERE singleton=1").fetchone()
                )
        except (OSError, sqlite3.Error):
            pass
        missing = not path.exists()
        integrity_state = "not_checked" if missing else "damaged"
        reason = (
            "missing_file"
            if missing
            else str(exc)
            if isinstance(exc, ScanError)
            else "SQLite integrity or schema check failed"
            if isinstance(exc, sqlite3.Error)
            else "scan artifact cannot be read"
        )
        return {
            "ok": False,
            "error": "scan artifact is missing" if missing else reason,
            "source": {"scan_uuid": scan_uuid} if scan_uuid else None,
            "frontier": {
                "state": "unavailable",
                "reason": "scan could not be opened",
                "counts": None,
            },
            "link_positions": {"state": "unavailable", "reason": "scan could not be opened"},
            "committed_page_outcomes": None,
            "validation": "not_checked",
            "integrity": {
                "state": integrity_state,
                "checked_at": checked_at if not missing else None,
                "damaged_pages": None,
                "affected_scan_ids": (
                    [scan_uuid] if scan_uuid and integrity_state == "damaged" else []
                ),
                "reason": reason,
            },
            "checkpoint_available": checkpoint_available,
        }
    try:
        scan = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        source = {
            key: scan[key]
            for key in (
                "scan_uuid",
                "format_version",
                "source_kind",
                "parent_scan_uuid",
                "writer_version",
                "writer_revision",
                "evidence_revision",
                "created_at",
                "finished_at",
                "lifecycle",
                "finish_reason",
            )
        }
        source.update(
            crawl_partial=bool(scan["crawl_partial"]), corpus_partial=bool(scan["corpus_partial"])
        )
        outcomes = {"2xx": 0, "3xx": 0, "4xx": 0, "5xx": 0, "other": 0, "no_response": 0}
        for code, count in con.execute(
            "SELECT status_code,COUNT(*) FROM pages GROUP BY status_code"
        ):
            if code is None:
                outcomes["no_response"] += count
            elif 200 <= code < 600:
                outcomes[f"{code // 100}xx"] += count
            else:
                outcomes["other"] += count
        if scan["source_kind"] == "legacy_import":
            frontier = {
                "state": "unavailable",
                "reason": "legacy import retains no native frontier",
                "counts": None,
            }
        else:
            counts = {state: 0 for state in ("queued", "inflight", "done", "excluded")}
            for state, count in con.execute("SELECT state,COUNT(*) FROM frontier GROUP BY state"):
                counts[state] = count
            frontier = {"state": "available", "reason": "", "counts": counts}
        checkpoint_available = bool(
            source["lifecycle"] in {"created", "running", "interrupted", "paused"}
            and con.execute("SELECT 1 FROM resume_state WHERE singleton=1").fetchone()
        )
        return {
            "ok": True,
            "source": source,
            "frontier": frontier,
            "link_positions": position_summary(con),
            "committed_page_outcomes": outcomes,
            "validation": validation,
            "integrity": {
                "state": "ok" if validation == "full" else "not_checked",
                "checked_at": checked_at if full_validation else None,
                "damaged_pages": 0 if validation == "full" else None,
                "affected_scan_ids": [],
                "reason": (
                    ""
                    if full_validation
                    else "full validation timestamp is not retained"
                    if validation == "full"
                    else "full validation was not requested"
                ),
            },
            "checkpoint_available": checkpoint_available,
        }
    finally:
        con.close()

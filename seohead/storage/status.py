"""Read-only operational status summaries for validated scan artifacts."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from . import open_scan_mode
from .link_positions import position_summary


def scan_status(input_path: str, *, full_validation: bool = False) -> dict[str, Any]:
    """Summarize one scan snapshot without fetching, retrying, or changing its bytes.

    By default the scan is accepted by a light header/schema check and the response says
    ``validation: "light"``; ``full_validation=True`` runs the complete artifact validation
    (``"full"``). Counters come from SQL either way.
    """
    if not isinstance(input_path, str) or not input_path:
        raise ValueError("input_path is required")
    con, validation = open_scan_mode(
        Path(input_path), require_audit=False, light=not full_validation
    )
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
        return {
            "ok": True,
            "source": source,
            "frontier": frontier,
            "link_positions": position_summary(con),
            "committed_page_outcomes": outcomes,
            "validation": validation,
        }
    finally:
        con.close()

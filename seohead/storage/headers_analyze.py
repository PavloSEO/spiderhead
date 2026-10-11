"""Analyze headers and transport phases already retained by a local scan."""

from __future__ import annotations

import json
import sqlite3
import time
from typing import Any

from . import open_scan_mode

FORMAT = "seohead.headers-analysis.v1"
_PHASES = ("dns", "connect", "tls", "ttfb", "download")


def analyze(
    input_path: str,
    *,
    url: str | None = None,
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1_048_576,
    timeout_seconds: float = 15.0,
) -> dict[str, Any]:
    """Read retained response headers and phase timings; never issue a request."""
    if not isinstance(input_path, str) or not input_path:
        raise ValueError("input_path is required")
    if url is not None and (not isinstance(url, str) or not url or len(url) > 8192):
        raise ValueError("url must be an exact retained URL of at most 8192 characters")
    if type(offset) is not int or not 0 <= offset <= 2**40:
        raise ValueError("offset must be 0..2^40")
    if type(limit) is not int or not 1 <= limit <= 200:
        raise ValueError("limit must be 1..200")
    if type(max_bytes) is not int or not 4096 <= max_bytes <= 8 * 1024 * 1024:
        raise ValueError("max_bytes must be 4096..8388608")
    if type(timeout_seconds) not in {int, float} or not 0.1 <= timeout_seconds <= 30:
        raise ValueError("timeout_seconds must be 0.1..30")
    try:
        con, validation = open_scan_mode(input_path, require_audit=False, light=True)
    except (ValueError, OSError, sqlite3.Error) as exc:
        return {
            "ok": False,
            "state": "unavailable",
            "format": FORMAT,
            "reason_code": "scan_not_available",
            "error": str(exc),
        }
    try:
        source_row = con.execute(
            "SELECT scan_uuid,format_version,source_kind,evidence_revision,lifecycle,"
            "finish_reason,crawl_partial,corpus_partial FROM scan WHERE singleton=1"
        ).fetchone()
        if source_row is None:
            raise ValueError("scan identity is unavailable")
        source = dict(source_row)
        if source["source_kind"] != "native":
            return {
                "ok": True,
                "state": "unavailable",
                "format": FORMAT,
                "reason": "saved source does not retain native response headers",
                "source": source,
                "validation": validation,
            }
        names = {row[0] for row in con.execute("SELECT name FROM main.sqlite_master WHERE type='table'")}
        transport_columns = {
            row[1] for row in con.execute("PRAGMA table_info(response_transport_meta)")
        } if "response_transport_meta" in names else set()
        has_phase_data = all(
            f"{phase}_{suffix}" in transport_columns
            for phase in _PHASES
            for suffix in ("seconds", "state", "reason")
        )
        phase_select = (
            ",t.protocol,t.protocol_state," + ",".join(
                f"t.{phase}_{suffix} AS {phase}_{suffix}"
                for phase in _PHASES
                for suffix in ("seconds", "state", "reason")
            )
            if has_phase_data
            else ",t.protocol,t.protocol_state"
            if "response_transport_meta" in names
            else ",NULL AS protocol,'unavailable' AS protocol_state"
        )
        phase_join = (
            " LEFT JOIN response_transport_meta t ON t.response_id=r.response_id"
            if "response_transport_meta" in names
            else ""
        )
        where = " WHERE u.url=?" if url is not None else ""
        params = [url] if url is not None else []
        deadline = time.monotonic() + timeout_seconds
        con.set_progress_handler(lambda: int(time.monotonic() >= deadline), 10_000)
        try:
            total = con.execute(
                "SELECT COUNT(*) FROM pages p JOIN urls u USING(url_id)" + where,
                params,
            ).fetchone()[0]
            if url is not None and total == 0:
                return {
                    "ok": True,
                    "state": "not_found",
                    "format": FORMAT,
                    "source": source,
                    "url": url,
                    "total": 0,
                    "items": [],
                    "validation": validation,
                }
            rows = con.execute(
                "SELECT p.url_id,p.page_ordinal,p.document_id,p.status_code,p.content_type,u.url,"
                "r.response_id,r.response_time,r.effective_status_code,r.effective_headers_redacted_json"
                + phase_select
                + " FROM pages p JOIN urls u USING(url_id) LEFT JOIN documents d ON d.document_id=p.document_id "
                "LEFT JOIN responses r ON r.response_id=COALESCE(d.source_response_id,("
                "SELECT MAX(r2.response_id) FROM responses r2 WHERE r2.request_url_id=p.url_id "
                "AND r2.purpose='page'))"
                + phase_join
                + where
                + " ORDER BY p.page_ordinal LIMIT ? OFFSET ?",
                (*params, limit, offset),
            ).fetchall()
            items = []
            output_limited = False
            findings_by_check: dict[str, int] = {}
            measured = unavailable = not_applicable = 0
            transport_counts = {phase: {} for phase in _PHASES}
            used = 0
            from seohead.crawl.security_headers import evaluate_page

            for row in rows:
                raw_headers = row["effective_headers_redacted_json"]
                analysis = evaluate_page(
                    row["url"],
                    row["effective_status_code"] or row["status_code"],
                    row["content_type"],
                    raw_headers,
                )
                if row["response_id"] is None and analysis["state"] == "measured":
                    analysis = {
                        "state": "unavailable",
                        "reason": "no retained response header record",
                        "headers": None,
                        "findings": [],
                    }
                if analysis["state"] == "measured":
                    measured += 1
                elif analysis["state"] == "not_applicable":
                    not_applicable += 1
                else:
                    unavailable += 1
                for finding in analysis["findings"]:
                    findings_by_check[finding["check"]] = findings_by_check.get(finding["check"], 0) + 1
                phases = {}
                for phase in _PHASES:
                    state_key = f"{phase}_state"
                    phases[phase] = (
                        {
                            "seconds": row[f"{phase}_seconds"],
                            "state": row[state_key] if row[state_key] != "unknown" else "unavailable",
                            "reason": row[f"{phase}_reason"] or "",
                        }
                        if has_phase_data and row["response_id"] is not None
                        else {
                            "seconds": None,
                            "state": "unavailable",
                            "reason": "transport phase was not retained",
                        }
                    )
                    state = phases[phase]["state"]
                    transport_counts[phase][state] = transport_counts[phase].get(state, 0) + 1
                item = {
                    "url": row["url"],
                    "url_id": row["url_id"],
                    "page_ordinal": row["page_ordinal"],
                    "response_id": row["response_id"],
                    "status_code": row["effective_status_code"] or row["status_code"],
                    "total_seconds": row["response_time"],
                    "http_version": row["protocol"],
                    "http_version_state": row["protocol_state"] or "unavailable",
                    "headers_state": analysis["state"],
                    "headers_reason": analysis["reason"],
                    "headers": analysis["headers"],
                    "findings": analysis["findings"],
                    "transport": phases,
                }
                size = len(json.dumps(item, ensure_ascii=False).encode("utf-8"))
                if used + size > max_bytes:
                    output_limited = True
                    break
                items.append(item)
                used += size
        finally:
            con.set_progress_handler(None, 0)
        returned = len(items)
        if output_limited and returned == 0:
            return {
                "ok": True,
                "state": "limit_reached",
                "format": FORMAT,
                "reason": "one header record exceeds max_bytes",
                "source": source,
                "offset": offset,
                "limit": limit,
                "total": total,
                "items": [],
                "validation": validation,
            }
        next_offset = offset + returned
        has_more = next_offset < total
        complete = (
            source["lifecycle"] == "finished"
            and not source["crawl_partial"]
            and not source["corpus_partial"]
            and not output_limited
            and returned == min(limit, max(0, total - offset))
        )
        return {
            "ok": True,
            "state": "available" if complete else "partial",
            "format": FORMAT,
            "source": source,
            "offset": offset,
            "limit": limit,
            "total": total,
            "total_state": "exact",
            "returned": returned,
            "has_more": has_more,
            "next_offset": next_offset if has_more else None,
            "bytes": used,
            "summary": {
                "headers_measured": measured,
                "headers_unavailable": unavailable,
                "not_applicable": not_applicable,
                "findings_by_check": findings_by_check,
                "transport_phase_states": transport_counts,
            },
            "items": items,
            "validation": validation,
            "scope": "retained redacted response headers and transport metadata; no network request",
        }
    except sqlite3.OperationalError as exc:
        if "interrupt" in str(exc).lower():
            return {
                "ok": True,
                "state": "limit_reached",
                "format": FORMAT,
                "reason": "read_time_budget_exceeded",
                "source": source,
                "items": [],
                "validation": validation,
            }
        return {
            "ok": False,
            "state": "unavailable",
            "format": FORMAT,
            "reason_code": "scan_read_failed",
            "error": str(exc),
            "source": source,
            "validation": validation,
        }
    finally:
        con.set_progress_handler(None, 0)
        con.close()

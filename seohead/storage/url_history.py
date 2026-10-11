"""Per-URL state of one URL across every retained scan of a project, read-only.

Each scan is opened with the light identity and schema check, and the URL is looked up by its exact
retained text through the unique index on ``urls.url``. A scan that never committed a page for the
URL reports ``state: absent``; an unreadable scan is listed in ``errors`` and never mistaken for
absence. Nothing is written, and the scans are never modified.
"""

from __future__ import annotations

import hashlib
import sqlite3
from pathlib import Path
from typing import Any

from . import ScanError, open_scan_mode

FORMAT = "seohead.scan-url-history.v1"
DEFAULT_LIMIT = 50
MAX_LIMIT = 500
MAX_URL_LENGTH = 8192

# One indexed lookup per scan. indexability mirrors the url-query rule: 2xx and no noindex in
# meta robots or X-Robots-Tag. The document's body hash is the page's content identity.
_PAGE_SQL = (
    "SELECT p.status_code,"
    " COALESCE(p.status_code BETWEEN 200 AND 299 AND"
    " instr(lower(p.meta_robots||','||p.x_robots),'noindex')=0, 0) AS indexable,"
    " p.title,p.meta_description,p.h1,p.canonical,p.redirect_url,d.body_sha256,"
    " p.size_bytes,p.response_time,p.word_count,"
    " (SELECT COUNT(*) FROM links l WHERE l.source_url_id=p.url_id) AS outlinks,"
    " (SELECT COUNT(*) FROM links l WHERE l.destination_url_id=u.url_id) AS inlinks"
    " FROM urls u JOIN pages p ON p.url_id=u.url_id"
    " LEFT JOIN documents d ON d.document_id=p.document_id"
    " WHERE u.url=?"
)


def _text_or_none(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _observe(path: Path, url: str) -> dict[str, Any]:
    """Return the page state of ``url`` in one scan, or ``None`` when it has no page there."""
    con, _mode = open_scan_mode(path, require_audit=False, light=True)
    try:
        row = con.execute(_PAGE_SQL, (url,)).fetchone()
    finally:
        con.close()
    if row is None:
        return None
    title = row["title"] if isinstance(row["title"], str) else ""
    return {
        "status_code": row["status_code"],
        "indexability": bool(row["indexable"]),
        "title": title,
        "title_hash": hashlib.sha256(title.encode("utf-8")).hexdigest(),
        "meta_description": row["meta_description"],
        "h1": row["h1"],
        "canonical": _text_or_none(row["canonical"]),
        "redirect_target": _text_or_none(row["redirect_url"]),
        "content_hash": _text_or_none(row["body_sha256"]),
        "word_count": row["word_count"],
        "inlinks": row["inlinks"],
        "outlinks": row["outlinks"],
        "response_time_seconds": row["response_time"],
        "html_size_bytes": row["size_bytes"],
    }


_EMPTY_STATE: dict[str, Any] = {
    "status_code": None,
    "indexability": None,
    "title_hash": None,
    "title": None,
    "meta_description": None,
    "h1": None,
    "canonical": None,
    "redirect_target": None,
    "content_hash": None,
    "word_count": None,
    "inlinks": None,
    "outlinks": None,
    "response_time_seconds": None,
    "html_size_bytes": None,
}

_CHANGE_FIELDS = (
    "status_code",
    "title",
    "meta_description",
    "h1",
    "word_count",
    "inlinks",
    "outlinks",
    "canonical",
    "response_time_seconds",
    "indexability",
    "html_size_bytes",
)


def _transitions(scans: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Compare adjacent observations without treating an absent URL as a fix."""
    transitions = []
    for newer, older in zip(scans, scans[1:]):
        entry = {
            "before_scan_uuid": older["scan_uuid"],
            "after_scan_uuid": newer["scan_uuid"],
        }
        if newer["state"] != "present" or older["state"] != "present":
            entry.update(
                state="not_comparable",
                reason="both scans must contain a readable observation of this URL",
                changes={},
            )
        else:
            entry.update(
                state="compared",
                changes={
                    field: {"before": older[field], "after": newer[field]}
                    for field in _CHANGE_FIELDS
                    if older[field] != newer[field]
                },
            )
        transitions.append(entry)
    return transitions


def url_history(scans_dir: str | Path, url: str, *, limit: int = DEFAULT_LIMIT) -> dict[str, Any]:
    """Return the state of ``url`` in the newest ``limit`` scans of the directory, newest first.

    ``number`` counts every scan of the directory from 1 for the oldest, so it stays stable when
    the window of returned scans changes.
    """
    from seohead.storage.history import _catalog

    if not isinstance(url, str) or not url or len(url) > MAX_URL_LENGTH:
        raise ValueError("url must be nonempty exact retained URL text of at most 8192 characters")
    if type(limit) is not int or not 1 <= limit <= MAX_LIMIT:
        raise ValueError(f"limit must be an integer 1..{MAX_LIMIT}")
    directory = Path(scans_dir)
    items, catalog_errors = _catalog(directory)
    total = len(items)
    scans: list[dict[str, Any]] = []
    errors: list[dict[str, Any]] = list(catalog_errors)
    for index, item in enumerate(items[:limit]):
        entry: dict[str, Any] = {
            "scan_uuid": item["uuid"],
            "number": total - index,
            "finished_at": item["finished_at"],
        }
        try:
            found = _observe(Path(item["path"]), url)
        except (ScanError, sqlite3.Error, OSError, ValueError) as exc:
            entry.update(state="unavailable", **_EMPTY_STATE)
            errors.append({"scan_uuid": item["uuid"], "reason": str(exc)[:2048]})
        else:
            if found is None:
                entry.update(state="absent", **_EMPTY_STATE)
            else:
                entry.update(state="present", **found)
        scans.append(entry)
    return {
        "ok": True,
        "state": "available" if not errors else "partial",
        "format": FORMAT,
        "url": url,
        "scan_total": total,
        "limit": limit,
        "returned": len(scans),
        "has_more": total > len(scans),
        "scans": scans,
        "transitions": _transitions(scans),
        "errors": errors,
    }

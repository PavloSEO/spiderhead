"""Per-URL state of one URL across every retained scan of a project, read-only.

Each scan is opened with the light identity and schema check, and the URL is looked up by its exact
retained text through the unique index on ``urls.url``. A scan that never committed a page for the
URL reports ``state: absent``; an unreadable scan is listed in ``errors`` and never mistaken for
absence. Nothing is written, and the scans are never modified.
"""

from __future__ import annotations

import hashlib
import itertools
import json
import sqlite3
from collections import Counter
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from . import ScanError, open_scan_mode

FORMAT = "seohead.scan-url-history.v1"
DEFAULT_LIMIT = 50
MAX_LIMIT = 500
MAX_URL_LENGTH = 8192

# One indexed lookup per scan. indexability mirrors the url-query rule: 2xx and no noindex in
# meta robots or X-Robots-Tag. The document's body hash is the page's content identity.
_PAGE_SQL = (
    "SELECT p.url_id,p.status_code,"
    " COALESCE(p.status_code BETWEEN 200 AND 299 AND"
    " instr(lower(p.meta_robots||','||p.x_robots),'noindex')=0, 0) AS indexable,"
    " p.title,p.meta_description,p.h1,p.canonical,p.redirect_url,d.body_sha256,d.body_state,d.body_reason,"
    " p.size_bytes,p.response_time,p.word_count,"
    " (SELECT COUNT(*) FROM links l WHERE l.source_url_id=p.url_id) AS outlinks,"
    " (SELECT COUNT(*) FROM links l WHERE l.destination_url_id=u.url_id) AS inlinks"
    " FROM urls u JOIN pages p ON p.url_id=u.url_id"
    " LEFT JOIN documents d ON d.document_id=p.document_id"
    " WHERE u.url=?"
)


def _text_or_none(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _external_link_response_codes(con: sqlite3.Connection, url_id: int) -> dict[str, Any]:
    """Report responses only for linked external targets that the scan actually fetched."""
    from seohead.crawl.spider import Scope

    try:
        scan = con.execute(
            "SELECT start_url,config_json,capabilities_json,limitations_json,crawl_partial "
            "FROM scan WHERE singleton=1"
        ).fetchone()
        config = json.loads(scan["config_json"])
        capabilities = json.loads(scan["capabilities_json"])
        links = capabilities.get("links") if isinstance(capabilities, dict) else None
        link_complete = isinstance(links, dict) and links.get("state") == "complete"
        link_meta = links if isinstance(links, dict) else {}
        link_reason = (
            ""
            if link_complete
            else link_meta.get("reason", "complete link observations are unavailable")
        )
        discovery = config.get("discovery") if isinstance(config, dict) else None
        external_policy = (discovery or {}).get("external") if isinstance(discovery, dict) else None
        if isinstance(external_policy, dict) and external_policy.get("store") is False:
            return {"state": "unavailable", "reason": "external links were not retained"}
        scope = Scope.from_config(config.get("scope"))
        host = urlsplit(scan["start_url"] or "").hostname or ""
    except (sqlite3.Error, TypeError, ValueError, KeyError, json.JSONDecodeError) as exc:
        return {"state": "unavailable", "reason": f"scan scope is unavailable: {str(exc)[:512]}"}

    totals: Counter[int] = Counter()
    external = observed = 0
    for row in con.execute(
        "SELECT d.url,l.destination_url_id,"
        "(SELECT COALESCE(r.effective_status_code,r.status_code) FROM responses r "
        "WHERE r.request_url_id=l.destination_url_id AND r.purpose='page' "
        "ORDER BY r.request_ordinal DESC LIMIT 1) AS status_code "
        "FROM links l JOIN urls d ON d.url_id=l.destination_url_id "
        "WHERE l.source_url_id=? GROUP BY l.destination_url_id ORDER BY d.url",
        (url_id,),
    ):
        try:
            internal = scope.is_internal(row["url"], host)
        except (TypeError, ValueError):
            continue
        if internal:
            continue
        external += 1
        status = row["status_code"]
        if type(status) is int and 100 <= status <= 599:
            totals[status] += 1
            observed += 1
    complete = link_complete and observed == external
    state = "measured" if complete else "partial" if external or observed else "unavailable"
    reason = (
        "one or more linked external targets have no retained page response"
        if observed < external
        else link_reason or "external-link coverage is incomplete"
        if not link_complete
        else ""
    )
    return {
        "state": state,
        "external_targets": external if link_complete else None,
        "external_targets_observed": external,
        "responses_measured": observed,
        "codes": {str(code): count for code, count in sorted(totals.items())},
        **({"reason": reason} if reason else {}),
    }


def _observe(path: Path, url: str) -> dict[str, Any]:
    """Return the page state of ``url`` in one scan, or ``None`` when it has no page there."""
    con, _mode = open_scan_mode(path, require_audit=False, light=True)
    try:
        row = con.execute(_PAGE_SQL, (url,)).fetchone()
        scan = con.execute(
            "SELECT capabilities_json,crawl_partial FROM scan WHERE singleton=1"
        ).fetchone()
        capabilities = json.loads(scan["capabilities_json"])
        links = capabilities.get("links") if isinstance(capabilities, dict) else None
        links_state = links.get("state") if isinstance(links, dict) else "unavailable"
        crawl_partial = bool(scan["crawl_partial"])
        external_statuses = (
            _external_link_response_codes(con, row["url_id"]) if row is not None else None
        )
    finally:
        con.close()
    if row is None:
        return None
    title = row["title"] if isinstance(row["title"], str) else ""
    link_count_state = (
        "measured"
        if links_state == "complete" and not crawl_partial
        else "partial"
        if links_state != "unavailable"
        else "unavailable"
    )
    canonical = _text_or_none(row["canonical"])
    redirect = _text_or_none(row["redirect_url"])
    body_reason = row["body_reason"] or "document was not retained"
    field_states = {
        "status_code": {
            "state": "measured" if row["status_code"] is not None else "unavailable",
            **(
                {}
                if row["status_code"] is not None
                else {"reason": "response status was not retained"}
            ),
        },
        "indexability": {
            "state": "measured" if row["status_code"] is not None else "unavailable",
            **(
                {}
                if row["status_code"] is not None
                else {"reason": "response status is required to determine indexability"}
            ),
        },
        "title": {"state": "measured"},
        "meta_description": {"state": "measured"},
        "h1": {"state": "measured"},
        "word_count": {"state": "measured"},
        "canonical": {"state": "measured" if canonical else "absent"},
        "redirect_target": {"state": "measured" if redirect else "absent"},
        "content_hash": {
            "state": "measured" if row["body_sha256"] else "unavailable",
            **({} if row["body_sha256"] else {"reason": body_reason}),
        },
        "response_time_seconds": {
            "state": "measured" if row["response_time"] is not None else "unavailable",
            **(
                {}
                if row["response_time"] is not None
                else {"reason": "response timing was not retained"}
            ),
        },
        "html_size_bytes": {
            "state": "measured" if row["status_code"] is not None else "unavailable",
            **(
                {}
                if row["status_code"] is not None
                else {"reason": "response size was not retained"}
            ),
        },
        "inlinks": {
            "state": link_count_state,
            **(
                {}
                if link_count_state == "measured"
                else {
                    "reason": "link inventory is incomplete"
                    if link_count_state == "partial"
                    else "link inventory is unavailable"
                }
            ),
        },
        "outlinks": {
            "state": link_count_state,
            **(
                {}
                if link_count_state == "measured"
                else {
                    "reason": "link inventory is incomplete"
                    if link_count_state == "partial"
                    else "link inventory is unavailable"
                }
            ),
        },
        "external_link_response_codes": external_statuses,
    }
    return {
        "status_code": row["status_code"],
        "indexability": bool(row["indexable"]) if row["status_code"] is not None else None,
        "title": title,
        "title_hash": hashlib.sha256(title.encode("utf-8")).hexdigest(),
        "meta_description": row["meta_description"],
        "h1": row["h1"],
        "canonical": canonical,
        "redirect_target": redirect,
        "content_hash": _text_or_none(row["body_sha256"]),
        "word_count": row["word_count"],
        "inlinks": row["inlinks"] if link_count_state == "measured" else None,
        "outlinks": row["outlinks"] if link_count_state != "unavailable" else None,
        "response_time_seconds": row["response_time"],
        "html_size_bytes": row["size_bytes"] if row["status_code"] is not None else None,
        "field_states": field_states,
        "external_link_response_codes": external_statuses,
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
    "external_link_response_codes": None,
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
    "external_link_response_codes",
)


def _transitions(scans: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Compare adjacent observations without treating an absent URL as a fix."""
    transitions = []
    for newer, older in itertools.pairwise(scans):
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
            changes = {}
            for field in _CHANGE_FIELDS:
                before_value, after_value = older[field], newer[field]
                before_state = older.get("field_states", {}).get(field, {}).get("state")
                after_state = newer.get("field_states", {}).get(field, {}).get("state")
                if before_state in {"unavailable", "partial"} or after_state in {
                    "unavailable",
                    "partial",
                }:
                    changes[field] = {
                        "state": "unavailable",
                        "reason": "field evidence is incomplete on at least one scan",
                    }
                elif before_value != after_value:
                    changes[field] = {"before": before_value, "after": after_value}
            entry.update(
                state="compared",
                changes=changes,
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

"""scan.v2 declared-resource graph, independent of the frozen v1 script/style lane."""

from __future__ import annotations

import json
import re
import sqlite3
import time
import warnings
from collections.abc import Iterable
from io import BytesIO
from typing import Any
from urllib.parse import urljoin, urlsplit

from bs4 import BeautifulSoup
from PIL import Image, UnidentifiedImageError

from . import ScanError

KIND = "resource_graph_coverage"
_REPRESENTATIONS = {"static", "rendered", "legacy_fragment"}
_STATES = {
    "complete",
    "partial",
    "unavailable",
    "disabled",
    "empty",
    "excluded",
    "failed",
    "budget",
}
_CSS_URL = re.compile(r"url\(\s*(['\"]?)(.*?)\1\s*\)", re.IGNORECASE)
_CSS_IMPORT = re.compile(r"@import\s+(?:url\()?\s*['\"]([^'\"]+)['\"]", re.IGNORECASE)
_INTEGRITY_STATES = {"declared", "absent", "unknown"}


def ensure_schema(con: sqlite3.Connection) -> None:
    """Create the optional v2 resource extension only in an explicit v2 writer."""
    if con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return
    con.execute(
        "CREATE TABLE IF NOT EXISTS resource_graph_occurrences (occurrence_id INTEGER PRIMARY KEY,"
        "page_url_id INTEGER NOT NULL,source_document_id INTEGER NOT NULL,representation TEXT NOT NULL,"
        "ordinal INTEGER NOT NULL,kind TEXT NOT NULL,carrier TEXT NOT NULL,raw_url TEXT NOT NULL,"
        "resolved_url TEXT NOT NULL,integrity TEXT,integrity_state TEXT NOT NULL DEFAULT 'unknown',"
        "nesting_depth INTEGER NOT NULL,state TEXT NOT NULL,reason TEXT NOT NULL,"
        "UNIQUE(page_url_id,source_document_id,representation,ordinal))"
    )
    occurrence_columns = {
        row[1] for row in con.execute("PRAGMA table_info(resource_graph_occurrences)")
    }
    for name, definition in (
        ("integrity", "TEXT"),
        ("integrity_state", "TEXT NOT NULL DEFAULT 'unknown'"),
    ):
        if name not in occurrence_columns:
            con.execute(f"ALTER TABLE resource_graph_occurrences ADD COLUMN {name} {definition}")
    con.execute(
        "CREATE TABLE IF NOT EXISTS resource_graph_fetches (resolved_url TEXT PRIMARY KEY,state TEXT NOT NULL,"
        "reason TEXT NOT NULL,status_code INTEGER,content_type TEXT NOT NULL,bytes_received INTEGER NOT NULL,"
        "elapsed_seconds REAL,origin_host TEXT NOT NULL,redirects INTEGER NOT NULL,nesting_depth INTEGER NOT NULL)"
    )
    existing = {row[1] for row in con.execute("PRAGMA table_info(resource_graph_fetches)")}
    for name, definition in (
        ("final_url", "TEXT NOT NULL DEFAULT ''"),
        ("compression", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("cache_state", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("integrity_state", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("width", "INTEGER"),
        ("height", "INTEGER"),
        ("body_state", "TEXT NOT NULL DEFAULT 'unavailable'"),
        ("delivery_json", "TEXT"),
    ):
        if name not in existing:
            con.execute(f"ALTER TABLE resource_graph_fetches ADD COLUMN {name} {definition}")
    con.execute(
        "CREATE INDEX IF NOT EXISTS resource_graph_occurrences_url ON resource_graph_occurrences(resolved_url)"
    )
    con.execute(
        "CREATE INDEX IF NOT EXISTS resource_graph_pending ON resource_graph_occurrences(occurrence_id) WHERE state='disabled'"
    )


def _append(
    values: list[dict[str, Any]],
    *,
    kind: str,
    carrier: str,
    raw: str,
    base_url: str,
    integrity: str | None = None,
    integrity_state: str = "unknown",
) -> None:
    value = raw.strip()
    resolved = urljoin(base_url, value)
    parts = urlsplit(resolved)
    if not value or parts.scheme.lower() not in {"http", "https"} or not parts.hostname:
        return
    values.append(
        {
            "kind": kind,
            "carrier": carrier,
            "raw_url": value,
            "resolved_url": resolved,
            "integrity": integrity,
            "integrity_state": integrity_state,
        }
    )


def _srcset(
    values: list[dict[str, Any]],
    raw: str,
    base_url: str,
    *,
    integrity: str | None,
    integrity_state: str,
) -> None:
    for candidate in raw.split(","):
        value = candidate.strip().split(maxsplit=1)[0] if candidate.strip() else ""
        _append(
            values,
            kind="image",
            carrier="srcset",
            raw=value,
            base_url=base_url,
            integrity=integrity,
            integrity_state=integrity_state,
        )


def extract(
    html: str | None, base_url: str, *, max_occurrences: int = 20_000
) -> tuple[list[dict[str, Any]], int]:
    """Extract declared HTTP(S) resources and inline CSS references in document order."""
    if not isinstance(html, str):
        return [], 0
    soup = BeautifulSoup(html, features="lxml")
    from seohead.checks.parser import document_base_url

    base_url = document_base_url(soup, base_url)
    values: list[dict[str, Any]] = []
    attributes = {
        "img": (("src", "image"), ("srcset", "image")),
        "source": (("src", "media"), ("srcset", "image")),
        "video": (("src", "media"), ("poster", "image")),
        "audio": (("src", "media"),),
        "track": (("src", "media"),),
        "iframe": (("src", "document"),),
        "embed": (("src", "document"),),
        "object": (("data", "document"),),
        "script": (("src", "module"),),
    }
    for tag in soup.find_all(True):
        name = tag.name.lower()
        declared_integrity = tag.get("integrity") if tag.has_attr("integrity") else None
        integrity = declared_integrity if isinstance(declared_integrity, str) else None
        integrity_state = (
            "declared"
            if integrity is not None
            else "unknown"
            if tag.has_attr("integrity")
            else "absent"
        )
        if name == "link":
            rel = {str(value).lower() for value in tag.get("rel") or ()}
            kind = (
                "stylesheet"
                if "stylesheet" in rel
                else "manifest"
                if "manifest" in rel
                else "module"
                if "modulepreload" in rel
                else None
            )
            if kind and isinstance(tag.get("href"), str):
                _append(
                    values,
                    kind=kind,
                    carrier="link[href]",
                    raw=tag["href"],
                    base_url=base_url,
                    integrity=integrity,
                    integrity_state=integrity_state,
                )
        for attribute, kind in attributes.get(name, ()):
            raw = tag.get(attribute)
            if not isinstance(raw, str):
                continue
            if attribute == "srcset":
                _srcset(
                    values,
                    raw,
                    base_url,
                    integrity=integrity,
                    integrity_state=integrity_state,
                )
            else:
                if name == "script" and str(tag.get("type") or "").lower() != "module":
                    kind = "script"
                _append(
                    values,
                    kind=kind,
                    carrier=f"{name}[{attribute}]",
                    raw=raw,
                    base_url=base_url,
                    integrity=integrity,
                    integrity_state=integrity_state,
                )
        style = tag.get("style")
        if isinstance(style, str):
            _css(values, style, base_url, carrier=f"{name}[style]")
        if name == "style":
            _css(values, tag.get_text(), base_url, carrier="style")
    omitted = max(0, len(values) - max_occurrences)
    return values[:max_occurrences], omitted


def _css(values: list[dict[str, Any]], text: str, base_url: str, *, carrier: str) -> None:
    for raw in _CSS_IMPORT.findall(text):
        _append(values, kind="css_import", carrier=carrier, raw=raw, base_url=base_url)
    for _quote, raw in _CSS_URL.findall(text):
        kind = (
            "font"
            if urlsplit(raw).path.lower().endswith((".woff", ".woff2", ".ttf", ".otf"))
            else "css_url"
        )
        _append(values, kind=kind, carrier=carrier, raw=raw, base_url=base_url)


def store_document(
    con: sqlite3.Connection,
    *,
    page_url_id: int,
    source_document_id: int,
    representation: str,
    html: str | None,
) -> dict[str, Any] | None:
    """Persist one representation's declaration population within its capture transaction."""
    if con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return None
    ensure_schema(con)
    if representation not in _REPRESENTATIONS:
        raise ScanError("resource graph representation is invalid")
    document = con.execute(
        "SELECT url_id,representation FROM documents WHERE document_id=?", (source_document_id,)
    ).fetchone()
    if document is None or tuple(document) != (page_url_id, representation):
        raise ScanError("resource graph document binding is invalid")
    url = con.execute("SELECT url FROM urls WHERE url_id=?", (page_url_id,)).fetchone()[0]
    values, omitted = extract(html, url)
    con.execute(
        "DELETE FROM resource_graph_occurrences WHERE page_url_id=? AND source_document_id=? AND representation=?",
        (page_url_id, source_document_id, representation),
    )
    state = (
        "unavailable"
        if html is None
        else "partial"
        if omitted
        else "empty"
        if not values
        else "complete"
    )
    reason = (
        "document body was not available"
        if html is None
        else "resource declaration cap omitted occurrences"
        if omitted
        else ""
    )
    for ordinal, value in enumerate(values):
        con.execute(
            "INSERT INTO resource_graph_occurrences(page_url_id,source_document_id,representation,ordinal,kind,carrier,raw_url,resolved_url,integrity,integrity_state,nesting_depth,state,reason) "
            "VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                page_url_id,
                source_document_id,
                representation,
                ordinal,
                value["kind"],
                value["carrier"],
                value["raw_url"],
                value["resolved_url"],
                value["integrity"],
                value["integrity_state"],
                0,
                "disabled",
                "resource fetch is disabled",
            ),
        )
    payload = {
        "schema_version": "resource_graph.v1",
        "page_url_id": page_url_id,
        "source_document_id": source_document_id,
        "representation": representation,
        "observed": len(values),
        "omitted": omitted,
        "state": state,
        "reason": reason,
    }
    con.execute(
        "INSERT OR REPLACE INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) VALUES(?,?,?,?,?,?)",
        (
            KIND,
            f"document:{source_document_id}",
            "scan_context.v1",
            json.dumps(payload, sort_keys=True, separators=(",", ":")),
            "unavailable"
            if state == "unavailable"
            else "partial"
            if state == "partial"
            else "complete",
            reason,
        ),
    )
    return payload


def validate_coverage_context(con: sqlite3.Connection, item: dict[str, str], payload: Any) -> None:
    """Validate one document-bound declaration coverage envelope without extraction."""
    if (
        not isinstance(payload, dict)
        or set(payload)
        != {
            "schema_version",
            "page_url_id",
            "source_document_id",
            "representation",
            "observed",
            "omitted",
            "state",
            "reason",
        }
        or payload["schema_version"] != "resource_graph.v1"
        or type(payload["page_url_id"]) is not int
        or payload["page_url_id"] < 1
        or type(payload["source_document_id"]) is not int
        or payload["source_document_id"] < 1
        or payload["representation"] not in _REPRESENTATIONS
        or type(payload["observed"]) is not int
        or payload["observed"] < 0
        or type(payload["omitted"]) is not int
        or payload["omitted"] < 0
        or payload["state"] not in {"complete", "partial", "unavailable", "empty"}
        or not isinstance(payload["reason"], str)
        or item["item_key"] != f"document:{payload['source_document_id']}"
        or not con.execute(
            "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
            (payload["source_document_id"], payload["page_url_id"], payload["representation"]),
        ).fetchone()
    ):
        raise ScanError("resource graph coverage context is invalid")
    direct, nested, invalid_nested = con.execute(
        "SELECT SUM(nesting_depth=0),SUM(nesting_depth>0),"
        "SUM(nesting_depth>0 AND carrier!='css') FROM resource_graph_occurrences "
        "WHERE page_url_id=? AND source_document_id=? AND representation=?",
        (payload["page_url_id"], payload["source_document_id"], payload["representation"]),
    ).fetchone()
    direct, nested, invalid_nested = int(direct or 0), int(nested or 0), int(invalid_nested or 0)
    state, observed, omitted, reason = (
        payload["state"],
        payload["observed"],
        payload["omitted"],
        payload["reason"],
    )
    expected_completeness = (
        "unavailable" if state == "unavailable" else "partial" if state == "partial" else "complete"
    )
    if (
        invalid_nested
        or observed != direct
        or item["completeness"] != expected_completeness
        or item["reason"] != reason
        or (
            state == "unavailable"
            and (
                observed
                or omitted
                or direct
                or nested
                or reason != "document body was not available"
            )
        )
        or (state == "empty" and (observed or omitted or direct or nested or reason))
        or (state == "complete" and (not observed or omitted or reason))
        or (
            state == "partial"
            and (not omitted or reason != "resource declaration cap omitted occurrences")
        )
    ):
        raise ScanError("resource graph coverage context disagrees with stored declarations")


def read(con: sqlite3.Connection, *, limit: int = 1_000, offset: int = 0) -> dict[str, Any]:
    """Read typed resource declarations/fetches without re-extracting or fetching."""
    if con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return {"state": "unavailable", "reason": "resource graph was not stored in this scan"}
    names = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    if not {"resource_graph_occurrences", "resource_graph_fetches"} <= names:
        return {"state": "unavailable", "reason": "resource graph extension is absent"}
    if type(limit) is not int or type(offset) is not int or not 1 <= limit <= 10_000 or offset < 0:
        raise ValueError("resource graph limit must be 1..10000 and offset nonnegative")
    total = con.execute("SELECT COUNT(*) FROM resource_graph_occurrences").fetchone()[0]
    states = {
        row[0]: row[1]
        for row in con.execute(
            "SELECT state,COUNT(*) FROM resource_graph_occurrences GROUP BY state"
        )
    }
    occurrences_by_kind = {
        row[0]: row[1]
        for row in con.execute(
            "SELECT kind,COUNT(*) FROM resource_graph_occurrences GROUP BY kind ORDER BY kind"
        )
    }
    weights_by_kind = {
        row[0]: {
            "unique_urls": row[1],
            "complete_urls": row[2],
            "unmeasured_urls": row[1] - row[2],
            "bytes_received": row[3] if row[1] == row[2] else None,
            "bytes_state": "exact" if row[1] == row[2] else "partial" if row[2] else "unavailable",
        }
        for row in con.execute(
            "SELECT x.kind,COUNT(*),COALESCE(SUM(f.state='complete'),0),"
            "SUM(CASE WHEN f.state='complete' THEN f.bytes_received ELSE 0 END) "
            "FROM (SELECT DISTINCT kind,resolved_url FROM resource_graph_occurrences) x "
            "LEFT JOIN resource_graph_fetches f USING(resolved_url) GROUP BY x.kind ORDER BY x.kind"
        )
    }
    occurrences = [
        dict(row)
        for row in con.execute(
            "SELECT * FROM resource_graph_occurrences ORDER BY page_url_id,source_document_id,ordinal LIMIT ? OFFSET ?",
            (limit, offset),
        )
    ]
    fetches = [
        dict(row)
        for row in con.execute(
            "SELECT * FROM resource_graph_fetches ORDER BY resolved_url LIMIT ? OFFSET ?",
            (limit, offset),
        )
    ]
    coverage_rows = con.execute(
        "SELECT completeness,COUNT(*) FROM context_items WHERE kind='resource_graph_coverage' "
        "GROUP BY completeness"
    ).fetchall()
    coverage_counts = {row[0]: row[1] for row in coverage_rows}
    coverage = {
        "state": "unavailable"
        if not coverage_counts
        else "partial"
        if coverage_counts.get("partial") or coverage_counts.get("unavailable")
        else "complete",
        "documents": sum(coverage_counts.values()),
        "coverage_counts": coverage_counts,
        "occurrence_state_counts": states,
    }
    return {
        "state": coverage["state"],
        "coverage": coverage,
        "total": total,
        "offset": offset,
        "limit": limit,
        "has_more": offset + len(occurrences) < total,
        "occurrences": occurrences,
        "fetches": fetches,
        "aggregates": {
            "blocks_by_kind": occurrences_by_kind,
            "blocks_by_state": states,
            "occurrences_by_kind": occurrences_by_kind,
            "weight_by_kind": weights_by_kind,
            "unique_resource_weight_by_kind": weights_by_kind,
        },
    }


def validate(con: sqlite3.Connection) -> None:
    """Validate the optional v2 graph without imposing it on earlier v2 artifacts."""
    names = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    present = {"resource_graph_occurrences", "resource_graph_fetches"} & names
    if not present:
        return
    if present != {"resource_graph_occurrences", "resource_graph_fetches"}:
        raise ScanError("resource graph schema is incomplete")
    for row in con.execute("SELECT * FROM resource_graph_occurrences"):
        if (
            row["representation"] not in _REPRESENTATIONS
            or row["state"] not in _STATES
            or row["ordinal"] < 0
            or row["nesting_depth"] < 0
            or not row["kind"]
            or not row["carrier"]
            or not row["raw_url"]
            or not row["resolved_url"]
            or row["integrity_state"] not in _INTEGRITY_STATES
            or (row["integrity_state"] != "declared" and row["integrity"] is not None)
            or not con.execute(
                "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
                (row["source_document_id"], row["page_url_id"], row["representation"]),
            ).fetchone()
        ):
            raise ScanError("resource graph occurrence is invalid")
    if con.execute(
        "SELECT 1 FROM resource_graph_occurrences GROUP BY page_url_id,source_document_id,representation "
        "HAVING MIN(ordinal)!=0 OR MAX(ordinal)!=COUNT(*)-1 LIMIT 1"
    ).fetchone():
        raise ScanError("resource graph occurrence ordinals are not contiguous")
    for row in con.execute("SELECT * FROM resource_graph_fetches"):
        if "delivery_json" in row.keys() and row["delivery_json"] is not None:  # noqa: SIM118 -- sqlite3.Row membership checks values.
            try:
                if len(row["delivery_json"]) > 16 * 1024:
                    raise ValueError("delivery summary exceeds its bound")
                delivery = json.loads(row["delivery_json"])
                if (
                    not isinstance(delivery, dict)
                    or not isinstance(delivery.get("cache"), dict)
                    or type(delivery["cache"].get("ok")) is not bool
                    or (
                        delivery.get("font_display") is not None
                        and not isinstance(delivery["font_display"], list)
                    )
                    or type(delivery.get("unminified")) not in {bool, type(None)}
                    or type(delivery.get("legacy_js")) not in {bool, type(None)}
                ):
                    raise ValueError("invalid delivery summary")
            except (TypeError, ValueError) as exc:
                raise ScanError("resource delivery evidence is invalid") from exc
        if (
            row["state"] not in {"complete", "partial", "failed", "excluded", "budget"}
            or row["bytes_received"] < 0
            or row["redirects"] < 0
            or row["nesting_depth"] < 0
            or row["integrity_state"] not in _INTEGRITY_STATES | {"mixed"}
            or (row["width"] is None) != (row["height"] is None)
            or (row["width"] is not None and (type(row["width"]) is not int or row["width"] < 1))
            or (row["height"] is not None and (type(row["height"]) is not int or row["height"] < 1))
        ):
            raise ScanError("resource graph fetch is invalid")


def capture(
    scan: Any,
    settings: dict[str, Any],
    *,
    client: Any = None,
    fetcher: Any = None,
    wait: Any = None,
    clock: Any = time.monotonic,
) -> dict[str, int]:
    """Fetch deduplicated v2 declarations under resource-graph-specific budgets."""
    if scan.con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return {"stored": 0, "fetched": 0, "excluded": 0, "failed": 0, "budget": 0}
    ensure_schema(scan.con)
    graph = settings["resources"]["graph"]
    if not settings["resources"]["fetch"]:
        return {"stored": 0, "fetched": 0, "excluded": 0, "failed": 0, "budget": 0}
    from seohead.crawl.collect import fetch_one
    from seohead.crawl.spider import Scope
    from seohead.crawl.sqlite_adapter import _headers
    from seohead.crawl.throttle import RequestBudgetExhausted

    start_url = scan.con.execute("SELECT start_url FROM scan WHERE singleton=1").fetchone()[0]
    start_host = (urlsplit(start_url or "").hostname or "").lower()
    scope = Scope.from_config(settings["scope"])
    started = clock() if clock is not None else 0.0
    seen_origins: set[str] = set()
    used_count = used_bytes = 0
    totals = {"stored": 0, "fetched": 0, "excluded": 0, "failed": 0, "budget": 0}
    while True:
        row = scan.con.execute(
            "SELECT o.* FROM resource_graph_occurrences o WHERE o.state='disabled' "
            "ORDER BY o.occurrence_id LIMIT 1"
        ).fetchone()
        if row is None:
            break
        url = row["resolved_url"]
        host = (urlsplit(url).hostname or "").lower()
        reason = scope.rejection(url, start_host)
        if reason:
            _set_occurrence_state(scan.con, row["resolved_url"], "excluded", reason)
            totals["excluded"] += 1
            continue
        elapsed = (clock() - started) if clock is not None else 0.0
        if (
            used_count >= graph["max_requests"]
            or used_bytes >= graph["max_bytes"]
            or (graph["max_seconds"] and elapsed >= graph["max_seconds"])
            or (host not in seen_origins and len(seen_origins) >= graph["max_origins"])
            or row["nesting_depth"] > graph["max_nesting"]
        ):
            _set_occurrence_state(scan.con, url, "budget", "resource graph budget exhausted")
            totals["budget"] += 1
            continue
        existing = scan.con.execute(
            "SELECT state,reason FROM resource_graph_fetches WHERE resolved_url=?", (url,)
        ).fetchone()
        if existing is not None:
            _set_occurrence_state(scan.con, url, existing["state"], existing["reason"])
            totals["stored"] += 1
            continue
        events = []
        before = clock() if clock is not None else 0.0
        try:
            current = url
            record = None
            redirects = 0
            resource_bytes = 0
            redirect_scope_reason = ""
            graph_budget_reason = ""
            while True:
                elapsed = (clock() - started) if clock is not None else 0.0
                current_host = (urlsplit(current).hostname or "").lower()
                if used_count >= graph["max_requests"]:
                    graph_budget_reason = "resource graph request budget exhausted"
                    break
                if used_bytes >= graph["max_bytes"]:
                    graph_budget_reason = "resource graph byte budget exhausted"
                    break
                if graph["max_seconds"] and elapsed >= graph["max_seconds"]:
                    graph_budget_reason = "resource graph wall-clock budget exhausted"
                    break
                if current_host not in seen_origins and len(seen_origins) >= graph["max_origins"]:
                    graph_budget_reason = "resource graph origin budget exhausted"
                    break
                response_limit = min(
                    graph["max_bytes_per_resource"] - resource_bytes,
                    graph["max_bytes"] - used_bytes,
                )
                if response_limit < 1:
                    graph_budget_reason = "resource graph byte budget exhausted"
                    break
                prior_events = len(events)
                record, _parsed = fetch_one(
                    current,
                    client=client,
                    fetcher=fetcher,
                    extra_headers=_headers(settings, current),
                    user_agent=settings["http"]["user_agent"],
                    max_response_bytes=response_limit,
                    retry_on_timeout=0,
                    wait=wait,
                    capture_observer=events.append,
                    capture_max_bytes=response_limit,
                )
                used_count += 1
                hop_bytes = sum(len(event.entity_bytes or b"") for event in events[prior_events:])
                resource_bytes += hop_bytes
                used_bytes += hop_bytes
                seen_origins.add(current_host)
                if not record.redirect_url or not 300 <= (record.status_code or 0) < 400:
                    break
                next_url = urljoin(current, record.redirect_url)
                redirect_reason = scope.rejection(next_url, start_host)
                if redirect_reason:
                    redirect_scope_reason = redirect_reason
                    break
                if redirects >= graph["max_redirects"]:
                    break
                redirects += 1
                current = next_url
            event = events[-1] if events else None
            body = event.entity_bytes if event is not None else None
            content_type = record.content_type if record is not None else ""
            status = record.status_code if record is not None else None
            if graph_budget_reason:
                state, reason = "budget", graph_budget_reason
            elif redirect_scope_reason:
                state, reason = "excluded", redirect_scope_reason
            elif (
                record is not None
                and record.redirect_url
                and 300 <= (record.status_code or 0) < 400
            ):
                state, reason = "failed", "resource redirect budget exhausted"
            elif status is None or not 200 <= status < 300:
                state, reason = (
                    "failed",
                    (record.error if record is not None else "")
                    or "resource response was not successful",
                )
            elif status == 206:
                state, reason = (
                    "partial",
                    "HTTP 206 response contains only a requested byte range; whole resource was not measured",
                )
            elif body is None or event.body_state != "complete":
                state, reason = "partial", "resource response body was not retained completely"
            else:
                state, reason = "complete", ""
            received = resource_bytes
            scan.con.execute(
                "INSERT INTO resource_graph_fetches(resolved_url,state,reason,status_code,content_type,bytes_received,elapsed_seconds,origin_host,redirects,nesting_depth,final_url,compression,cache_state,integrity_state,width,height,body_state) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    url,
                    state,
                    reason,
                    status,
                    content_type,
                    received,
                    (clock() - before) if clock else None,
                    host,
                    redirects,
                    row["nesting_depth"],
                    current,
                    (event.content_encoding if event is not None else "unknown") or "identity",
                    (record.cache_status if record is not None else "") or "unknown",
                    _fetch_integrity_state(scan.con, url),
                    *_image_dimensions(body),
                    "complete" if state == "complete" else "partial",
                ),
            )
            _set_occurrence_state(scan.con, url, state, reason)
            if state == "complete" and event is not None and body is not None:
                from seohead.checks.asset_weight import resource_delivery_evidence

                delivery = resource_delivery_evidence(body, row["kind"], event.response_headers)
                scan.con.execute(
                    "UPDATE resource_graph_fetches SET delivery_json=? WHERE resolved_url=?",
                    (json.dumps(delivery), url),
                )
            if state == "complete":
                totals["fetched"] += 1
            elif state == "excluded":
                totals["excluded"] += 1
            elif state == "budget":
                totals["budget"] += 1
            else:
                totals["failed"] += 1
            if state == "complete" and content_type.partition(";")[0].strip().lower() == "text/css":
                _store_css_children(
                    scan.con, row, (body or b"").decode("utf-8", "replace"), graph["max_nesting"]
                )
        except RequestBudgetExhausted:
            _set_occurrence_state(scan.con, url, "budget", "total HTTP request budget exhausted")
            totals["budget"] += 1
            break
        except Exception as exc:
            used_count += 1
            scan.con.execute(
                "INSERT INTO resource_graph_fetches(resolved_url,state,reason,status_code,content_type,bytes_received,elapsed_seconds,origin_host,redirects,nesting_depth,final_url,compression,cache_state,integrity_state,width,height,body_state) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    url,
                    "failed",
                    str(exc)[:500],
                    None,
                    "",
                    0,
                    None,
                    host,
                    0,
                    row["nesting_depth"],
                    "",
                    "unknown",
                    "unknown",
                    _fetch_integrity_state(scan.con, url),
                    None,
                    None,
                    "unavailable",
                ),
            )
            _set_occurrence_state(scan.con, url, "failed", "resource fetch failed")
            totals["failed"] += 1
    scan.con.commit()
    return totals


def _set_occurrence_state(con: sqlite3.Connection, url: str, state: str, reason: str) -> None:
    con.execute(
        "UPDATE resource_graph_occurrences SET state=?,reason=? WHERE resolved_url=? AND state='disabled'",
        (state, reason, url),
    )


def invalidate_pages(con: sqlite3.Connection, page_url_ids: Iterable[int]) -> None:
    """Discard graph facts for requeued pages while retaining their captured corpus history."""
    page_ids = sorted({value for value in page_url_ids if type(value) is int and value > 0})
    if not page_ids:
        return
    tables = {
        row[0]
        for row in con.execute(
            "SELECT name FROM sqlite_master WHERE type='table' "
            "AND name IN ('resource_graph_occurrences','resource_graph_fetches')"
        )
    }
    if "resource_graph_occurrences" not in tables:
        return
    document_ids: set[int] = set()
    urls: set[str] = set()
    for chunk in _chunks(page_ids):
        placeholders = ",".join("?" for _ in chunk)
        document_ids.update(
            row[0]
            for row in con.execute(
                f"SELECT document_id FROM documents WHERE url_id IN ({placeholders})", chunk
            )
        )
        urls.update(
            row[0]
            for row in con.execute(
                f"SELECT DISTINCT resolved_url FROM resource_graph_occurrences "
                f"WHERE page_url_id IN ({placeholders})",
                chunk,
            )
        )
        con.execute(
            f"DELETE FROM resource_graph_occurrences WHERE page_url_id IN ({placeholders})", chunk
        )
    for chunk in _chunks(sorted(document_ids)):
        keys = [f"document:{document_id}" for document_id in chunk]
        placeholders = ",".join("?" for _ in keys)
        con.execute(
            f"DELETE FROM context_items WHERE kind=? AND item_key IN ({placeholders})",
            (KIND, *keys),
        )
    if "resource_graph_fetches" not in tables:
        return
    for chunk in _chunks(sorted(urls)):
        placeholders = ",".join("?" for _ in chunk)
        con.execute(
            f"DELETE FROM resource_graph_fetches WHERE resolved_url IN ({placeholders})", chunk
        )
        con.execute(
            f"UPDATE resource_graph_occurrences SET state='disabled',"
            "reason='resource fetch invalidated by requeue' "
            f"WHERE resolved_url IN ({placeholders})",
            chunk,
        )


def _chunks(values: list[Any], size: int = 500) -> Iterable[list[Any]]:
    for offset in range(0, len(values), size):
        yield values[offset : offset + size]


def _fetch_integrity_state(con: sqlite3.Connection, url: str) -> str:
    """Summarize declarations without losing their individual integrity values."""
    states = {
        row[0]
        for row in con.execute(
            "SELECT DISTINCT integrity_state FROM resource_graph_occurrences WHERE resolved_url=?",
            (url,),
        )
    }
    if len(states) == 1:
        return states.pop()
    return "mixed" if states else "unknown"


def _image_dimensions(body: bytes | None) -> tuple[int | None, int | None]:
    """Read visually oriented dimensions without decoding or transposing pixels."""
    if body is None:
        return None, None
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("error")
            with Image.open(BytesIO(body)) as image:
                width, height = image.size
                if image.format == "PNG":
                    # PNG metadata may follow IDAT. Pillow's PNG getexif() calls
                    # load() to reach it, decoding pixels; walk retained chunks instead.
                    offset = 8
                    while offset + 12 <= len(body):
                        length = int.from_bytes(body[offset : offset + 4], "big")
                        end = offset + length + 12
                        if end > len(body):
                            return None, None
                        kind = body[offset + 4 : offset + 8]
                        if kind == b"eXIf":
                            image.info["exif"] = body[offset + 8 : end - 4]
                        offset = end
                        if kind == b"IEND":
                            break
                    else:
                        return None, None
                orientation = Image.Image.getexif(image).get(274, 1)
                if type(orientation) is not int or not 1 <= orientation <= 8:
                    return None, None
                if orientation in {5, 6, 7, 8}:
                    width, height = height, width
    except (Image.DecompressionBombError, UnidentifiedImageError, OSError, ValueError, Warning):
        return None, None
    if type(width) is not int or type(height) is not int or width < 1 or height < 1:
        return None, None
    return width, height


def _store_css_children(con: sqlite3.Connection, parent: Any, text: str, max_nesting: int) -> None:
    depth = parent["nesting_depth"] + 1
    values: list[dict[str, Any]] = []
    _css(values, text, parent["resolved_url"], carrier="css")
    ordinal = con.execute(
        "SELECT COALESCE(MAX(ordinal)+1,0) FROM resource_graph_occurrences "
        "WHERE page_url_id=? AND source_document_id=? AND representation=?",
        (parent["page_url_id"], parent["source_document_id"], parent["representation"]),
    ).fetchone()[0]
    for value in values:
        con.execute(
            "INSERT OR IGNORE INTO resource_graph_occurrences(page_url_id,source_document_id,representation,ordinal,kind,carrier,raw_url,resolved_url,integrity,integrity_state,nesting_depth,state,reason) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                parent["page_url_id"],
                parent["source_document_id"],
                parent["representation"],
                ordinal,
                value["kind"],
                value["carrier"],
                value["raw_url"],
                value["resolved_url"],
                value["integrity"],
                value["integrity_state"],
                depth,
                "disabled",
                "resource fetch is pending",
            ),
        )
        ordinal += 1

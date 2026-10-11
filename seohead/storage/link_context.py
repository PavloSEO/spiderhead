"""Offline per-occurrence link context from one retained complete document."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any
from urllib.parse import urldefrag, urlsplit

from seohead.checks.link_context import MAX_ANCHORS, SCHEMA_VERSION, extract_occurrences
from seohead.crawl.spider import Scope

from . import ScanError, open_scan_mode
from .bodies import read_document


def _validate_limits(offset: int, limit: int, max_body_bytes: int, max_result_bytes: int) -> None:
    if (
        type(offset) is not int
        or offset < 0
        or type(limit) is not int
        or not 1 <= limit <= 500
        or type(max_body_bytes) is not int
        or not 1 <= max_body_bytes <= 8 * 1024 * 1024
        or type(max_result_bytes) is not int
        or not 1024 <= max_result_bytes <= 8 * 1024 * 1024
    ):
        raise ScanError(
            "link context requires offset >= 0, limit 1..500, body bytes 1..8388608, "
            "result bytes 1024..8388608"
        )


def _unavailable(link: dict[str, Any], reason: str) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "link_id": link["link_id"],
        "source_url_id": link["source_url_id"],
        "destination_url_id": link["destination_url_id"],
        "source_document_id": link["source_document_id"],
        "representation": link["evidence_representation"],
        "ordinal": link["ordinal"],
        "source_url": link["source_url"],
        "destination_url": link["destination_url"],
        "anchor": link["anchor"],
        "nofollow": bool(link["nofollow"]),
        "raw_href": link["raw_href"] or None,
        "stored_position": link["position"] or None,
        "state": "unavailable",
        "reason": reason,
        "placement": None,
        "heading": None,
        "heading_relation": None,
        "disclosure": None,
    }


def _links(con, document_id: int) -> list[dict[str, Any]]:
    return [
        dict(row)
        for row in con.execute(
            "SELECT l.link_id,l.source_url_id,l.destination_url_id,l.source_document_id,"
            "l.evidence_representation,l.ordinal,l.anchor,l.nofollow,l.position,l.raw_href,"
            "l.rel_json,l.target,"
            "s.url AS source_url,d.url AS destination_url "
            "FROM links l JOIN urls s ON s.url_id=l.source_url_id "
            "JOIN urls d ON d.url_id=l.destination_url_id "
            "WHERE l.source_document_id=? ORDER BY l.ordinal,l.link_id LIMIT ?",
            (document_id, MAX_ANCHORS + 1),
        )
    ]


def _final_url(con, document: dict[str, Any], source_url: str) -> str:
    if document["representation"] == "rendered":
        try:
            url_id = json.loads(document["renderer_json"])["final_url_id"]
        except (TypeError, ValueError, KeyError):
            return source_url
    else:
        response = con.execute(
            "SELECT effective_url_id FROM responses WHERE response_id=?",
            (document["source_response_id"],),
        ).fetchone()
        url_id = response[0] if response else None
    if url_id is None:
        return source_url
    row = con.execute("SELECT url FROM urls WHERE url_id=?", (url_id,)).fetchone()
    return row[0] if row else source_url


def _stored_candidates(
    extracted: list[dict[str, Any]], config: dict[str, Any], depth: int, start_host: str
) -> list[dict[str, Any]]:
    if depth >= config["limits"]["max_depth"]:
        return []
    scope = Scope.from_config(config["scope"])
    discovery = config["discovery"]
    selected = []
    for item in extracted:
        target = urldefrag(item["href"]).url
        external = scope.rejection(target, start_host) == "outside_host"
        should_store = (
            discovery["external"]["store"] if external else discovery["hyperlinks"]["store"]
        )
        if should_store:
            selected.append(item)
    return selected


def _derive(
    con, document_id: int, links: list[dict[str, Any]], max_body_bytes: int
) -> tuple[list[dict], dict]:
    document_row = con.execute(
        "SELECT * FROM documents WHERE document_id=?", (document_id,)
    ).fetchone()
    if document_row is None:
        raise ScanError("source document is absent")
    document = dict(document_row)
    if len(links) > MAX_ANCHORS:
        return [], {"state": "unavailable", "reason": "link_occurrence_budget_exhausted"}
    if any(
        link["source_url_id"] != document["url_id"]
        or link["evidence_representation"] != document["representation"]
        for link in links
    ):
        raise ScanError("link occurrence points to a different source document")
    scan = con.execute("SELECT config_json,start_url FROM scan WHERE singleton=1").fetchone()
    config = json.loads(scan["config_json"])
    reason = ""
    if document["body_state"] != "complete":
        reason = f"body_{document['body_state']}:{document['body_reason']}"
    elif document["representation"] != "rendered":
        response = con.execute(
            "SELECT content_type FROM responses WHERE response_id=?",
            (document["source_response_id"],),
        ).fetchone()
        media_type = (response[0] or "").split(";", 1)[0].strip().lower() if response else ""
        if media_type != "text/html":
            reason = f"unsupported_source_mime:{media_type or 'missing'}"
    if reason:
        return (
            [_unavailable(link, reason) for link in links],
            {"state": "unavailable", "reason": reason},
        )
    try:
        html = read_document(con, document_id, max_decoded_bytes=max_body_bytes)
    except (ScanError, UnicodeError) as exc:
        reason = str(exc)
        return (
            [_unavailable(link, reason) for link in links],
            {"state": "unavailable", "reason": reason},
        )
    source_url = (
        links[0]["source_url"]
        if links
        else con.execute("SELECT url FROM urls WHERE url_id=?", (document["url_id"],)).fetchone()[0]
    )
    final_url = _final_url(con, document, source_url)
    extracted = extract_occurrences(
        html,
        final_url,
        content_area=config.get("evidence", {}).get("content_area"),
        position_rules=config.get("link_position", {}).get("rules"),
        cap=MAX_ANCHORS,
    )
    try:
        selected = _stored_candidates(
            extracted["occurrences"],
            config,
            con.execute(
                "SELECT crawl_depth FROM pages WHERE url_id=?", (document["url_id"],)
            ).fetchone()[0],
            (urlsplit(scan["start_url"] or "").hostname or "").lower(),
        )
    except (KeyError, TypeError, ValueError):
        reason = "recorded_link_policy_unavailable"
        return (
            [_unavailable(link, reason) for link in links],
            {"state": "unavailable", "reason": reason},
        )
    attributes_captured = config.get("link_attributes", {}).get("capture") is True
    position_captured = config.get("link_position", {}).get("classify") is True
    if len(selected) != len(links) or any(
        item["href"] != link["destination_url"]
        or item["anchor"] != link["anchor"]
        or int(item["nofollow"]) != link["nofollow"]
        or ((position_captured or link["position"]) and item["position"] != link["position"])
        or (
            attributes_captured
            and (
                item["raw_href"] != link["raw_href"]
                or item["target"] != link["target"]
                or item["rel"] != json.loads(link["rel_json"])
            )
        )
        for item, link in zip(selected, links, strict=False)
    ):
        reason = "stored_links_do_not_replay_from_retained_document"
        return (
            [_unavailable(link, reason) for link in links],
            {"state": "unavailable", "reason": reason},
        )
    items = []
    for link, item in zip(links, selected, strict=True):
        context = _unavailable(link, "")
        context.update(
            state="measured",
            reason="",
            placement={
                "position": item["position"],
                "basis": item["placement_basis"],
                "matched_selector": item["matched_selector"],
                "matched_selector_truncated": item["matched_selector_truncated"],
                "content_root_strategy": item["content_root_strategy"],
                "dom_path": item["dom_path"],
                "dom_path_truncated": item["dom_path_truncated"],
                "stored_position": link["position"] or None,
            },
            heading={**item["heading"], "relation": item["heading_relation"]}
            if item["heading"]
            else None,
            heading_relation=item["heading_relation"],
            disclosure=item["disclosure"],
        )
        items.append(context)
    omitted = extracted["eligible_omitted"]
    return items, {
        "state": "partial" if omitted else "complete",
        "reason": "eligible anchor observations exceeded parser cap" if omitted else "",
        "eligible_total": extracted["eligible_total"],
        "eligible_omitted": omitted,
    }


def _document_page(
    con, document_id: int, offset: int, limit: int, max_body_bytes: int, max_result_bytes: int
) -> dict[str, Any]:
    source = con.execute(
        "SELECT d.url_id,d.representation,u.url FROM documents d "
        "JOIN urls u ON u.url_id=d.url_id WHERE d.document_id=?",
        (document_id,),
    ).fetchone()
    if source is None:
        raise ScanError("source document is absent")
    links = _links(con, document_id)
    if len(links) > MAX_ANCHORS:
        raise ScanError("source document exceeds the supported link occurrence cap")
    items, coverage = _derive(con, document_id, links, max_body_bytes)
    scan = con.execute("SELECT scan_uuid,evidence_revision FROM scan WHERE singleton=1").fetchone()
    page = []
    used_bytes = 0
    for item in items[offset : offset + limit]:
        projected = {
            **item,
            "scan_uuid": scan["scan_uuid"],
            "evidence_revision": scan["evidence_revision"],
            "document_coverage": coverage,
        }
        size = len(json.dumps(projected, ensure_ascii=False).encode("utf-8"))
        if used_bytes + size > max_result_bytes:
            if not page:
                raise ScanError("one link context record exceeds max_result_bytes")
            break
        page.append(projected)
        used_bytes += size
    return {
        "schema_version": SCHEMA_VERSION,
        "scan_uuid": scan["scan_uuid"],
        "evidence_revision": scan["evidence_revision"],
        "source_document_id": document_id,
        "source_url_id": source["url_id"],
        "source_url": source["url"],
        "representation": source["representation"],
        "coverage": coverage,
        "total": len(items),
        "offset": offset,
        "limit": limit,
        "max_result_bytes": max_result_bytes,
        "result_bytes": used_bytes,
        "items": page,
        "has_more": offset + len(page) < len(items),
        "next_offset": offset + len(page) if offset + len(page) < len(items) else None,
    }


def contexts_for_document(
    scan_path: str | Path,
    document_id: int,
    *,
    offset: int = 0,
    limit: int = 100,
    max_body_bytes: int = 5 * 1024 * 1024,
    max_result_bytes: int = 1_048_576,
) -> dict[str, Any]:
    """Inspect one document's stored link occurrences; no crawl or scan mutation."""
    _validate_limits(offset, limit, max_body_bytes, max_result_bytes)
    if type(document_id) is not int or document_id < 1:
        raise ScanError("source document ID must be a positive integer")
    con, validation = open_scan_mode(scan_path, require_audit=False, light=True)
    try:
        page = _document_page(con, document_id, offset, limit, max_body_bytes, max_result_bytes)
        return {**page, "validation": validation}
    finally:
        con.close()


def context_for_link(
    scan_path: str | Path,
    link_id: int,
    *,
    max_body_bytes: int = 5 * 1024 * 1024,
    max_result_bytes: int = 1_048_576,
) -> dict[str, Any]:
    """Return one occurrence's context, including an explicit unavailable state."""
    if type(link_id) is not int or link_id < 1:
        raise ScanError("link ID must be a positive integer")
    _validate_limits(0, 1, max_body_bytes, max_result_bytes)
    con, validation = open_scan_mode(scan_path, require_audit=False, light=True)
    try:
        row = con.execute(
            "SELECT l.link_id,l.source_url_id,l.destination_url_id,l.source_document_id,"
            "l.evidence_representation,l.ordinal,l.anchor,l.nofollow,l.position,l.raw_href,"
            "s.url AS source_url,d.url AS destination_url "
            "FROM links l JOIN urls s ON s.url_id=l.source_url_id "
            "JOIN urls d ON d.url_id=l.destination_url_id WHERE l.link_id=?",
            (link_id,),
        ).fetchone()
        if row is None:
            raise ScanError("link ID is not present in this scan")
        link = dict(row)
        if link["source_document_id"] is None:
            scan = con.execute(
                "SELECT scan_uuid,evidence_revision FROM scan WHERE singleton=1"
            ).fetchone()
            item = {
                **_unavailable(link, "source_document_unavailable"),
                "scan_uuid": scan["scan_uuid"],
                "evidence_revision": scan["evidence_revision"],
                "document_coverage": {
                    "state": "unavailable",
                    "reason": "source_document_unavailable",
                },
            }
            if len(json.dumps(item, ensure_ascii=False).encode("utf-8")) > max_result_bytes:
                raise ScanError("one link context record exceeds max_result_bytes")
            return {**item, "validation": validation}
        document_id = link["source_document_id"]
        ordinal = link["ordinal"]
        page = _document_page(con, document_id, ordinal, 1, max_body_bytes, max_result_bytes)
        if not page["items"] or page["items"][0]["link_id"] != link_id:
            raise ScanError("link ordinal does not identify its source document occurrence")
        return {**page["items"][0], "validation": validation}
    finally:
        con.close()

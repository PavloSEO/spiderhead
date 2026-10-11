"""Versioned canonical/pagination attributes without changing the frozen scan SQL."""

from __future__ import annotations

import json
from typing import Any

from . import MAX_RECORD_BYTES, ScanError

VERSION = "head_links.v1"


def context_item(*, page_url_id: int, source_document_id: int, representation: str, parsed: Any):
    measured = isinstance(parsed, dict) and "head_links" in parsed
    payload = {
        "schema_version": VERSION,
        "page_url_id": page_url_id,
        "source_document_id": source_document_id,
        "representation": representation,
        "declarations": parsed["head_links"] if measured else None,
    }
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"))
    oversized = len(encoded.encode("utf-8")) > MAX_RECORD_BYTES
    if oversized:
        payload["declarations"] = None
        encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"))
        measured = False
    return {
        "kind": "head_links",
        "item_key": f"page:{page_url_id}:document:{source_document_id}:representation:{representation}",
        "payload_version": "scan_context.v1",
        "payload_json": encoded,
        "completeness": "partial"
        if measured and len(parsed["head_links"]) >= 100
        else "complete"
        if measured
        else "unavailable",
        "reason": "raw head annotations exceed native context byte limit"
        if oversized
        else "head annotation limit reached"
        if measured and len(parsed["head_links"]) >= 100
        else ""
        if measured
        else "head annotations were not parsed as eligible HTML",
    }


def validate_context(con, item, payload):
    if (
        not isinstance(payload, dict)
        or set(payload)
        != {"schema_version", "page_url_id", "source_document_id", "representation", "declarations"}
        or payload["schema_version"] != VERSION
        or type(payload["page_url_id"]) is not int
        or not 1 <= payload["page_url_id"] < 2**63
        or type(payload["source_document_id"]) is not int
        or not 1 <= payload["source_document_id"] < 2**63
        or payload["representation"] not in {"static", "rendered", "legacy_fragment"}
    ):
        raise ScanError("native head annotation context is invalid")
    declarations = payload["declarations"]
    if declarations is not None and (
        not isinstance(declarations, list)
        or len(declarations) > 100
        or any(
            not isinstance(row, dict)
            or not {"rel", "href", "target"}
            <= set(row)
            <= {"rel", "href", "target", "hreflang", "lang", "media", "type"}
            or any(type(value) is not str for value in row.values())
            for row in declarations
        )
    ):
        raise ScanError("native head annotation declarations are invalid")
    measured = declarations is not None
    unavailable_reason = item["reason"] in {
        "head annotations were not parsed as eligible HTML",
        "raw head annotations exceed native context byte limit",
    }
    if (
        item["item_key"]
        != f"page:{payload['page_url_id']}:document:{payload['source_document_id']}:representation:{payload['representation']}"
        or item["completeness"]
        != (
            "partial"
            if measured and len(declarations) >= 100
            else "complete"
            if measured
            else "unavailable"
        )
        or item["reason"]
        != (
            "head annotation limit reached"
            if measured and len(declarations) >= 100
            else ""
            if measured
            else item["reason"]
            if unavailable_reason
            else "head annotations were not parsed as eligible HTML"
        )
        or not con.execute(
            "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
            (payload["source_document_id"], payload["page_url_id"], payload["representation"]),
        ).fetchone()
    ):
        raise ScanError("native head annotation document identity disagrees")


def active(con):
    """Stream annotations only from each captured page's selected document."""
    for url, payload in con.execute(
        "SELECT u.url,c.payload_json FROM pages p JOIN urls u USING(url_id) "
        "JOIN context_items c ON c.kind='head_links' AND c.item_key="
        "'page:'||p.url_id||':document:'||p.document_id||':representation:'||p.representation "
        "ORDER BY p.page_ordinal"
    ):
        evidence = json.loads(payload)
        declarations = evidence["declarations"]
        yield (
            url,
            None
            if declarations is None
            else [
                {**row, "source_document_id": evidence["source_document_id"]}
                for row in declarations
            ],
        )


def for_url(con, url):
    """Read one selected document's complete annotations through indexed URL identity."""
    row = con.execute(
        "SELECT c.payload_json FROM pages p JOIN urls u USING(url_id) "
        "JOIN context_items c ON c.kind='head_links' AND c.item_key="
        "'page:'||p.url_id||':document:'||p.document_id||':representation:'||p.representation "
        "WHERE u.url=? AND c.completeness='complete'",
        (url,),
    ).fetchone()
    if row is None:
        return None
    evidence = json.loads(row[0])
    return [
        {**declaration, "source_document_id": evidence["source_document_id"]}
        for declaration in evidence["declarations"]
    ]


def eligible_count(con):
    """Count the same fetched HTML source population as AuditContext.html_pages."""
    return con.execute(
        "SELECT COUNT(*) FROM pages WHERE status_code BETWEEN 200 AND 299 AND lower(content_type) LIKE '%html%'"
    ).fetchone()[0]

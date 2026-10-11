"""Compare supported response MIME declarations with retained body signatures (#1013)."""

from __future__ import annotations

import json
import sqlite3
from collections import OrderedDict
from collections.abc import Callable
from typing import Any

from seohead.storage import ScanError
from seohead.storage.bodies import read_document_body

CHECK_ID = "BODY_MIME_MISMATCH"
MAX_BODY_BYTES = 2 * 1024 * 1024
_CACHE_LIMIT = 128


def _signature(body: bytes) -> str | None:
    """Return a supported body family from a bounded prefix, or None if ambiguous."""
    prefix = body[:4096].lstrip(b"\xef\xbb\xbf\x00\t\r\n ")
    for _ in range(8):
        if not prefix.startswith(b"<!--"):
            break
        end = prefix.find(b"-->")
        if end < 0:
            return None
        prefix = prefix[end + 3 :].lstrip(b"\x00\t\r\n ")
    lower = prefix.lower()
    if lower.startswith((b"<!doctype html", b"<html", b"<head", b"<body")):
        return "html"
    if prefix.startswith((b"{", b"[")):
        try:
            json.loads(body[:MAX_BODY_BYTES])
        except (RecursionError, UnicodeDecodeError, ValueError):
            return None
        return "json"
    if prefix.startswith(b"%PDF-"):
        return "pdf"
    if prefix.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png"
    if prefix.startswith(b"\xff\xd8\xff"):
        return "jpeg"
    if prefix.startswith((b"GIF87a", b"GIF89a")):
        return "gif"
    if len(prefix) >= 12 and prefix[:4] == b"RIFF" and prefix[8:12] == b"WEBP":
        return "webp"
    if lower.startswith((b"<svg", b"<?xml")) and b"<svg" in lower[:4096]:
        return "svg"
    return None


_DECLARED_FAMILIES = {
    "text/html": "html",
    "application/xhtml+xml": "html",
    "application/json": "json",
    "text/json": "json",
    "application/pdf": "pdf",
    "image/png": "png",
    "image/jpeg": "jpeg",
    "image/gif": "gif",
    "image/webp": "webp",
    "image/svg+xml": "svg",
}


def _declared_family(value: Any) -> tuple[str, str] | None:
    if not isinstance(value, str) or not value.strip():
        return None
    declared = value.split(";", 1)[0].strip().lower()
    family = _DECLARED_FAMILIES.get(declared)
    return (declared, family) if family else None


_PAGE_DOCUMENTS_SQL = (
    "SELECT u.url,p.url_id,p.status_code,p.content_type,p.representation,"
    "CASE WHEN p.representation='static' THEN p.document_id "
    "WHEN p.representation!='static' THEN ("
    " SELECT d.document_id FROM documents d JOIN context_items c "
    " ON c.item_key='document:'||d.document_id AND c.kind='resource_inventory' "
    " WHERE d.url_id=p.url_id AND d.representation='static' "
    " AND NOT EXISTS(SELECT 1 FROM context_items later JOIN documents newer "
    " ON later.item_key='document:'||newer.document_id "
    " WHERE later.kind='resource_inventory' AND newer.url_id=d.url_id "
    " AND newer.representation=d.representation AND newer.document_id>d.document_id) "
    " ORDER BY d.document_id DESC LIMIT 1) ELSE NULL END AS source_document_id "
    "FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal"
)

_DOCUMENT_RESPONSE_SQL = (
    "SELECT d.url_id,d.representation,d.body_sha256,d.body_state,d.body_reason,"
    "d.source_response_id,d.fidelity,d.renderer_json,r.request_url_id,r.body_sha256,"
    "r.body_state,r.body_reason,r.body_fidelity,r.content_type,r.status_code,"
    "r.effective_status_code FROM documents d LEFT JOIN responses r "
    "ON r.response_id=d.source_response_id WHERE d.document_id=?"
)


def _unavailable_reason(exc: ScanError) -> str:
    message = str(exc).lower()
    if "byte limit" in message or "exceeds decoded byte limit" in message:
        return "oversized_body"
    if "body unavailable" in message or "not_in_corpus" in message:
        return "absent_body"
    return "invalid_document_or_response_lineage"


def evaluate(
    con: sqlite3.Connection,
    *,
    on_finding: Callable[[dict[str, Any]], None] | None = None,
) -> dict[str, Any]:
    """Stream per-page findings from each page's selected static document.

    The document reader verifies its source-response and body-hash lineage. For rendered pages,
    the selected static resource inventory is used because the serialized DOM is not a response
    body. Missing, incomplete, oversized, or ambiguous evidence is counted by reason.
    """
    if not isinstance(con, sqlite3.Connection):
        raise TypeError("body MIME evaluation requires a SQLite scan connection")
    pages_total = eligible = measured = emitted = 0
    reasons: dict[str, int] = {}
    body_retention_states: dict[str, int] = {}

    def unavailable(reason: str) -> None:
        reasons[reason] = reasons.get(reason, 0) + 1

    family_cache: OrderedDict[str, tuple[str | None, str | None]] = OrderedDict()
    for url, url_id, page_status, page_type, representation, document_id in con.execute(
        _PAGE_DOCUMENTS_SQL
    ):
        pages_total += 1
        if representation not in {"static", "rendered", "legacy_fragment"}:
            unavailable("missing_page_representation")
            continue
        if type(page_status) is not int or not 200 <= page_status < 300:
            continue
        if document_id is None:
            if not page_type:
                unavailable("missing_declaration")
            elif _declared_family(page_type) is None:
                unavailable("unsupported_family")
            else:
                eligible += 1
                unavailable("missing_selected_static_document")
            continue

        document = con.execute(_DOCUMENT_RESPONSE_SQL, (document_id,)).fetchone()
        if document is None:
            unavailable("missing_selected_static_document")
            continue
        (
            document_url_id,
            document_representation,
            document_sha,
            document_state,
            document_reason,
            response_id,
            fidelity,
            renderer_json,
            request_url_id,
            response_sha,
            response_state,
            response_reason,
            response_fidelity,
            response_type,
            response_status,
            effective_status,
        ) = document
        declaration = _declared_family(response_type)
        if declaration is None:
            unavailable("missing_declaration" if not response_type else "unsupported_family")
            continue
        declared, expected = declaration
        eligible += 1
        if document_state != "complete" or response_state != "complete":
            unavailable("absent_body")
            state = "/".join(
                str(value or "unknown")
                for value in (document_state, response_state, document_reason, response_reason)
            )
            body_retention_states[state] = body_retention_states.get(state, 0) + 1
            continue
        if not isinstance(document_sha, str) or response_sha != document_sha:
            unavailable("absent_or_mismatched_body_reference")
            continue
        if (
            document_url_id != url_id
            or document_representation != "static"
            or renderer_json != "{}"
            or type(response_id) is not int
            or request_url_id != document_url_id
            or response_fidelity != fidelity
        ):
            unavailable("incomplete_or_mismatched_document_response_binding")
            continue
        status = effective_status if isinstance(effective_status, int) else response_status
        if not isinstance(status, int) or not 200 <= status < 300:
            unavailable("non_successful_source_response")
            continue
        if document_sha in family_cache:
            observed, cached_reason = family_cache.pop(document_sha)
            family_cache[document_sha] = (observed, cached_reason)
            if cached_reason:
                unavailable(cached_reason)
                continue
        else:
            try:
                body = read_document_body(
                    con, document_id, max_decoded_bytes=MAX_BODY_BYTES
                )
            except ScanError as exc:
                reason = _unavailable_reason(exc)
                unavailable(reason)
                family_cache[document_sha] = (None, reason)
                if len(family_cache) > _CACHE_LIMIT:
                    family_cache.popitem(last=False)
                continue
            observed = _signature(body)
            cached_reason = "ambiguous_body_signature" if observed is None else None
            family_cache[document_sha] = (observed, cached_reason)
            if len(family_cache) > _CACHE_LIMIT:
                family_cache.popitem(last=False)
        if observed is None:
            unavailable("ambiguous_body_signature")
            continue
        measured += 1
        if observed != expected:
            if on_finding is not None:
                on_finding(
                    {
                        "target_url": url,
                        "status_code": status,
                        "declared_content_type": declared,
                        "observed_body_family": observed,
                        "body_sha256": document_sha,
                        "body_bytes_limit": MAX_BODY_BYTES,
                        "body_state": document_state,
                        "body_reason": document_reason,
                        "document_id": document_id,
                        "source_response_id": response_id,
                        "representation": document_representation,
                        "target_scope": "page_response_body",
                        "status_scope": "page_response",
                        "reason": "retained selected-document response body signature conflicts with its declared Content-Type",
                    }
                )
            emitted += 1
    return {
        "pages_total": pages_total,
        "pages_eligible": eligible,
        "pages_measured": measured,
        "pages_unavailable": sum(
            count
            for reason, count in reasons.items()
            if reason not in {"missing_declaration", "unsupported_family"}
        ),
        "unavailable_reasons": reasons,
        "body_retention_states": body_retention_states,
        "findings_count": emitted,
    }

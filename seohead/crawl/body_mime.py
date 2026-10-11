"""Compare supported response MIME declarations with retained body signatures (#1013)."""

from __future__ import annotations

import json
import sqlite3
from collections import OrderedDict
from typing import Any

from seohead.storage import ScanError
from seohead.storage.bodies import read_body

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


def evaluate(con: sqlite3.Connection) -> dict[str, Any]:
    """Check retained page response bodies without network access or DOM inference.

    Only well-known MIME families with an unambiguous leading signature are compared.
    Missing, incomplete, oversized, or ambiguous bodies are counted as unmeasured.
    """
    if not isinstance(con, sqlite3.Connection):
        raise TypeError("body MIME evaluation requires a SQLite scan connection")
    findings: list[dict[str, Any]] = []
    measured = unmeasured = 0
    family_cache: OrderedDict[str, str | None] = OrderedDict()
    rows = con.execute(
        "SELECT u.url,p.status_code,r.content_type,r.body_sha256,r.body_state,r.body_reason "
        "FROM pages p JOIN urls u USING(url_id) LEFT JOIN responses r ON r.response_id=("
        " SELECT MAX(response_id) FROM responses WHERE effective_url_id=p.url_id AND purpose='page'"
        ") ORDER BY p.page_ordinal"
    )
    for url, status, content_type, body_sha, body_state, body_reason in rows:
        declaration = _declared_family(content_type)
        if declaration is None or not isinstance(status, int) or not 200 <= status < 300:
            continue
        declared, expected = declaration
        if body_state != "complete" or not isinstance(body_sha, str):
            unmeasured += 1
            continue
        if body_sha in family_cache:
            observed = family_cache.pop(body_sha)
            family_cache[body_sha] = observed
        else:
            try:
                body = read_body(con, body_sha, max_decoded_bytes=MAX_BODY_BYTES)
            except ScanError:
                unmeasured += 1
                continue
            observed = _signature(body)
            family_cache[body_sha] = observed
            if len(family_cache) > _CACHE_LIMIT:
                family_cache.popitem(last=False)
        if observed is None:
            unmeasured += 1
            continue
        measured += 1
        if observed != expected:
            findings.append(
                {
                    "target_url": url,
                    "status_code": status,
                    "declared_content_type": declared,
                    "observed_body_family": observed,
                    "body_sha256": body_sha,
                    "body_bytes_limit": MAX_BODY_BYTES,
                    "body_state": body_state,
                    "body_reason": body_reason,
                    "target_scope": "page_response_body",
                    "status_scope": "page_response",
                    "reason": "retained response body signature conflicts with its declared Content-Type",
                }
            )
    return {
        "findings": findings,
        "pages_measured": measured,
        "pages_unmeasured": unmeasured,
    }

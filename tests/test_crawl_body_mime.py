"""Body MIME findings use retained response bytes, not extension guesses (#1013)."""

from __future__ import annotations

import hashlib
import sqlite3

from seohead.crawl import body_mime


def _scan(pages):
    con = sqlite3.connect(":memory:")
    con.executescript(
        """
        CREATE TABLE urls (url_id INTEGER PRIMARY KEY, url TEXT NOT NULL);
        CREATE TABLE pages (
          url_id INTEGER PRIMARY KEY, page_ordinal INTEGER NOT NULL,
          status_code INTEGER, content_type TEXT NOT NULL);
        CREATE TABLE responses (
          response_id INTEGER PRIMARY KEY, effective_url_id INTEGER,
          purpose TEXT NOT NULL, content_type TEXT NOT NULL, body_sha256 TEXT,
          body_state TEXT NOT NULL, body_reason TEXT NOT NULL);
        CREATE TABLE bodies (
          sha256 TEXT PRIMARY KEY, codec TEXT NOT NULL, decoded_bytes INTEGER NOT NULL,
          stored_bytes INTEGER NOT NULL, data BLOB NOT NULL);
        """
    )
    for ordinal, (url, declared, body, state) in enumerate(pages, 1):
        con.execute("INSERT INTO urls VALUES (?,?)", (ordinal, url))
        con.execute(
            "INSERT INTO pages VALUES (?,?,200,?)", (ordinal, ordinal, declared)
        )
        body_sha = hashlib.sha256(body).hexdigest() if body is not None else None
        if body is not None:
            con.execute(
                "INSERT INTO bodies VALUES (?,?,?,?,?)",
                (body_sha, "identity", len(body), len(body), body),
            )
        con.execute(
            "INSERT INTO responses VALUES (?,?, 'page', ?, ?, ?, ?)",
            (ordinal, ordinal, declared, body_sha, state, "none" if body else "not_in_corpus"),
        )
    return con


def test_mismatch_uses_the_exact_retained_page_response():
    con = _scan(
        [("https://example.test/file", "text/html; charset=utf-8", b"%PDF-1.7\n", "complete")]
    )
    try:
        result = body_mime.evaluate(con)
    finally:
        con.close()

    assert result["pages_measured"] == 1
    assert result["pages_unmeasured"] == 0
    assert result["findings"] == [
        {
            "target_url": "https://example.test/file",
            "status_code": 200,
            "declared_content_type": "text/html",
            "observed_body_family": "pdf",
            "body_sha256": hashlib.sha256(b"%PDF-1.7\n").hexdigest(),
            "body_bytes_limit": body_mime.MAX_BODY_BYTES,
            "body_state": "complete",
            "body_reason": "none",
            "target_scope": "page_response_body",
            "status_scope": "page_response",
            "reason": "retained response body signature conflicts with its declared Content-Type",
        }
    ]


def test_missing_or_ambiguous_bodies_are_not_clean_results():
    con = _scan(
        [
            ("https://example.test/omitted", "application/json", None, "omitted"),
            ("https://example.test/unknown", "image/png", b"not a PNG", "complete"),
        ]
    )
    try:
        result = body_mime.evaluate(con)
    finally:
        con.close()

    assert result["findings"] == []
    assert result["pages_measured"] == 0
    assert result["pages_unmeasured"] == 2


def test_unrecognized_declared_type_is_outside_the_sniffer_contract():
    con = _scan(
        [("https://example.test/plain", "text/plain", b"<html>not judged</html>", "complete")]
    )
    try:
        result = body_mime.evaluate(con)
    finally:
        con.close()

    assert result == {"findings": [], "pages_measured": 0, "pages_unmeasured": 0}

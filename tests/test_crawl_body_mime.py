"""Body MIME findings bind to selected documents and bounded retained bytes (#1013)."""

from __future__ import annotations

import hashlib
import sqlite3

from seohead.crawl import body_mime


def _scan(
    body: bytes | None,
    content_type: str,
    *,
    final_url: str | None = None,
    competing: tuple[str, bytes] | None = None,
    body_state: str = "complete",
):
    con = sqlite3.connect(":memory:")
    con.executescript(
        """
        CREATE TABLE urls (url_id INTEGER PRIMARY KEY, url TEXT NOT NULL);
        CREATE TABLE pages (
          url_id INTEGER PRIMARY KEY, page_ordinal INTEGER NOT NULL,
          document_id INTEGER, representation TEXT NOT NULL, status_code INTEGER,
          content_type TEXT NOT NULL);
        CREATE TABLE responses (
          response_id INTEGER PRIMARY KEY, request_url_id INTEGER, effective_url_id INTEGER,
          purpose TEXT NOT NULL, status_code INTEGER, effective_status_code INTEGER,
          content_type TEXT, body_sha256 TEXT, body_state TEXT NOT NULL, body_reason TEXT NOT NULL,
          body_fidelity TEXT);
        CREATE TABLE documents (
          document_id INTEGER PRIMARY KEY, url_id INTEGER NOT NULL, representation TEXT NOT NULL,
          source_response_id INTEGER, body_sha256 TEXT, captured_at TEXT NOT NULL,
          decoder_version TEXT NOT NULL, decoder_source TEXT NOT NULL, decoder_charset TEXT NOT NULL,
          decoder_errors TEXT NOT NULL, fidelity TEXT NOT NULL, body_state TEXT NOT NULL,
          body_reason TEXT NOT NULL, renderer_json TEXT NOT NULL);
        CREATE TABLE bodies (
          sha256 TEXT PRIMARY KEY, codec TEXT NOT NULL, decoded_bytes INTEGER NOT NULL,
          stored_bytes INTEGER NOT NULL, data BLOB NOT NULL);
        CREATE TABLE context_items (item_key TEXT NOT NULL, kind TEXT NOT NULL);
        """
    )
    requested_url = "https://example.test/start"
    final_url = final_url or requested_url
    con.execute("INSERT INTO urls VALUES (1,?)", (requested_url,))
    if final_url != requested_url:
        con.execute("INSERT INTO urls VALUES (2,?)", (final_url,))
    sha = hashlib.sha256(body).hexdigest() if body is not None else None
    if body is not None:
        con.execute("INSERT INTO bodies VALUES (?,?,?,?,?)", (sha, "identity", len(body), len(body), body))
    con.execute(
        "INSERT INTO responses VALUES (1,1,?,'page',200,200,?,?,?,?,?)",
        (
            2 if final_url != requested_url else 1,
            content_type,
            sha,
            body_state,
            "none" if body is not None else "not_in_corpus",
            "entity_bytes",
        ),
    )
    con.execute(
        "INSERT INTO documents VALUES (1,1,'static',1,?,'2026-10-12T00:00:00Z',"
        "'scan_decoder.v1','utf8_fallback','utf-8','replace','entity_bytes',?,?, '{}')",
        (
            sha,
            body_state,
            "none" if body is not None else "not_in_corpus",
        ),
    )
    con.execute(
        "INSERT INTO pages VALUES (1,1,1,'static',200,?)", (content_type,)
    )
    if competing is not None:
        other_type, other_body = competing
        other_sha = hashlib.sha256(other_body).hexdigest()
        con.execute(
            "INSERT INTO bodies VALUES (?,?,?,?,?)",
            (other_sha, "identity", len(other_body), len(other_body), other_body),
        )
        con.execute(
            "INSERT INTO responses VALUES (2,2,1,'page',200,200,?,?, 'complete','none','entity_bytes')",
            (other_type, other_sha),
        )
    return con, requested_url


def test_selected_document_wins_over_competing_response_and_redirect_target():
    con, requested_url = _scan(
        b"%PDF-1.7\n",
        "text/html",
        final_url="https://example.test/final",
        competing=("application/json", b'{"wrong":"response"}'),
    )
    findings = []
    try:
        result = body_mime.evaluate(con, on_finding=findings.append)
    finally:
        con.close()

    assert result["pages_measured"] == 1
    assert result["findings_count"] == 1
    assert findings[0]["target_url"] == requested_url
    assert findings[0]["source_response_id"] == 1
    assert findings[0]["body_sha256"] == hashlib.sha256(b"%PDF-1.7\n").hexdigest()
    assert findings[0]["declared_content_type"] == "text/html"
    assert findings[0]["observed_body_family"] == "pdf"


def test_redirected_effective_url_does_not_hide_selected_response():
    con, requested_url = _scan(
        b"%PDF-1.7\n", "text/html", final_url="https://example.test/final"
    )
    findings = []
    try:
        result = body_mime.evaluate(con, on_finding=findings.append)
    finally:
        con.close()

    assert result["findings_count"] == 1
    assert findings[0]["target_url"] == requested_url
    assert findings[0]["source_response_id"] == 1


def test_missing_and_ambiguous_bodies_have_distinct_unavailable_counters():
    missing, _url = _scan(None, "application/json", body_state="omitted")
    ambiguous, _url = _scan(b"not a PNG", "image/png")
    try:
        missing_result = body_mime.evaluate(missing)
        ambiguous_result = body_mime.evaluate(ambiguous)
    finally:
        missing.close()
        ambiguous.close()

    assert missing_result["unavailable_reasons"] == {"absent_body": 1}
    assert missing_result["body_retention_states"] == {
        "omitted/omitted/not_in_corpus/not_in_corpus": 1
    }
    assert ambiguous_result["unavailable_reasons"] == {"ambiguous_body_signature": 1}


def test_unsupported_content_type_is_counted_outside_the_sniffed_family_set():
    con, _url = _scan(b"<html>unknown family</html>", "text/plain")
    try:
        result = body_mime.evaluate(con)
    finally:
        con.close()

    assert result["pages_eligible"] == 0
    assert result["unavailable_reasons"] == {"unsupported_family": 1}


def test_missing_declaration_and_oversized_body_keep_separate_counts():
    missing, _url = _scan(b"<html>no header</html>", "")
    oversized, _url = _scan(b"x" * (body_mime.MAX_BODY_BYTES + 1), "text/html")
    try:
        missing_result = body_mime.evaluate(missing)
        oversized_result = body_mime.evaluate(oversized)
    finally:
        missing.close()
        oversized.close()

    assert missing_result["unavailable_reasons"] == {"missing_declaration": 1}
    assert oversized_result["unavailable_reasons"] == {"oversized_body": 1}

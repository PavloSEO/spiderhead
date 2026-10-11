"""Exact, bounded retained-body readers over the scan.v1 body tables."""

from __future__ import annotations

import hashlib
import sqlite3
import zlib
from pathlib import Path

import pytest

from seohead.storage import ScanError
from seohead.storage.bodies import (
    MAX_WINDOW_CHARS,
    decode_entity,
    encode_body,
    read_body,
    read_document,
    read_page_document_content,
    read_document_window,
    _line_count,
)


def _con() -> sqlite3.Connection:
    con = sqlite3.connect(":memory:")
    con.executescript(Path("seohead/storage/scan_v1.sql").read_text(encoding="utf-8"))
    con.execute("INSERT INTO urls(url_id,url) VALUES(1,'https://example.test/')")
    return con


def _body(con: sqlite3.Connection, raw: bytes) -> str:
    row = encode_body(raw)
    con.execute(
        "INSERT INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(:sha256,:codec,:decoded_bytes,:stored_bytes,:data)",
        row,
    )
    return row["sha256"]  # type: ignore[return-value]


def _response(con: sqlite3.Connection, body_sha: str) -> None:
    con.execute(
        "INSERT INTO responses(response_id,request_url_id,request_ordinal,effective_url_id,"
        "redirect_chain_json,method,purpose,requested_at,request_headers_redacted_json,"
        "credentials_used,variant_key,response_headers_redacted_json,effective_headers_redacted_json,"
        "content_type,charset,content_encoding,transport_source,cache_status,body_sha256,"
        "body_fidelity,body_state,body_reason,error,error_kind) "
        "VALUES(1,1,1,1,'[]','GET','page','2026-01-01T00:00:00Z','[]',0,'','[]','[]',"
        "'text/html; charset=iso-8859-1','','','network','',?,'entity_bytes','complete','none','','')",
        (body_sha,),
    )


def _document(con: sqlite3.Connection, body_sha: str, **overrides) -> int:
    row = {
        "document_id": 1,
        "url_id": 1,
        "representation": "static",
        "source_response_id": 1,
        "body_sha256": body_sha,
        "captured_at": "2026-01-01T00:00:00Z",
        "decoder_version": "scan_decoder.v1",
        "decoder_source": "content_type_charset",
        "decoder_charset": "iso8859-1",
        "decoder_errors": "replace",
        "fidelity": "entity_bytes",
        "body_state": "complete",
        "body_reason": "none",
        "renderer_json": "{}",
    }
    row.update(overrides)
    names = ",".join(row)
    con.execute(
        f"INSERT INTO documents({names}) VALUES({','.join('?' for _ in row)})",
        tuple(row.values()),
    )
    return row["document_id"]


def test_encode_identity_zlib_dedup_and_zero_bytes():
    assert encode_body(b"")["codec"] == "identity"
    assert encode_body(b"")["sha256"] == hashlib.sha256(b"").hexdigest()
    raw = b"same bytes " * 200
    assert encode_body(raw)["codec"] == "zlib"
    assert encode_body(raw) == encode_body(raw)


def test_pre_g_empty_body_table_is_explicitly_unavailable():
    with pytest.raises(ScanError, match="unavailable: not_in_corpus"):
        read_body(_con(), hashlib.sha256(b"absent").hexdigest(), max_decoded_bytes=1)


def test_read_body_verifies_exact_bytes_and_compressed_corruption():
    con = _con()
    sha = _body(con, b"payload " * 200)
    assert read_body(con, sha, max_decoded_bytes=10_000) == b"payload " * 200
    encoded = encode_body(b"payload " * 200)
    con.execute("DELETE FROM bodies WHERE sha256=?", (sha,))
    con.execute(
        "INSERT INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(?,?,?,?,?)",
        (
            encoded["sha256"],
            encoded["codec"],
            encoded["decoded_bytes"],
            encoded["stored_bytes"] + 1,
            encoded["data"] + b"\x00",
        ),
    )
    with pytest.raises(ScanError, match=r"length|trailing|SHA"):
        read_body(con, sha, max_decoded_bytes=10_000)


def test_reader_rejects_oversized_encoded_metadata_without_selecting_blob():
    con = _con()
    sha = "a" * 64
    con.execute(
        "INSERT INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(?,?,?,?,?)",
        (sha, "identity", 1, 2, b"xx"),
    )
    statements = []
    con.set_trace_callback(statements.append)
    with pytest.raises(ScanError, match="stored size"):
        read_body(con, sha, max_decoded_bytes=1)
    assert not any("SELECT data" in statement for statement in statements)


@pytest.mark.parametrize(
    "data, message", [(b"not zlib", "invalid zlib"), (zlib.compress(b"abc")[:-1], "truncated")]
)
def test_reader_rejects_malformed_and_truncated_zlib_streams(data, message):
    con = _con()
    sha = "b" * 64
    con.execute(
        "INSERT INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(?,?,?,?,?)",
        (sha, "zlib", 10, len(data), data),
    )
    with pytest.raises(ScanError, match=message):
        read_body(con, sha, max_decoded_bytes=10)


def test_read_body_rejects_declared_or_actual_zlib_bombs():
    con = _con()
    raw = b"x" * 100_000
    sha = _body(con, raw)
    with pytest.raises(ScanError, match="limit"):
        read_body(con, sha, max_decoded_bytes=100)
    con.execute("UPDATE bodies SET decoded_bytes=1 WHERE sha256=?", (sha,))
    with pytest.raises(ScanError, match=r"limit|length|stored size"):
        read_body(con, sha, max_decoded_bytes=100_000)


def test_entity_decoder_uses_valid_charset_or_utf8_fallback_with_replacement():
    assert decode_entity(b"caf\xe9", "text/html; charset=ISO-8859-1")[0] == "café"
    text, metadata = decode_entity(b"\xff", "text/html; charset=not-a-codec")
    assert text == "�"
    assert metadata == {
        "decoder_version": "scan_decoder.v1",
        "decoder_source": "utf8_fallback",
        "decoder_charset": "utf-8",
        "decoder_errors": "replace",
    }
    assert decode_entity(b"\xff", "application/octet-stream")[0] == "�"


def test_read_document_validates_entity_lineage_and_named_unavailable_state():
    con = _con()
    sha = _body(con, b"caf\xe9")
    _response(con, sha)
    document_id = _document(con, sha)
    assert read_document(con, document_id, max_decoded_bytes=100) == "café"
    con.execute("UPDATE responses SET body_state='unavailable' WHERE response_id=1")
    with pytest.raises(ScanError, match=r"state.*lineage"):
        read_document(con, document_id, max_decoded_bytes=100)
    con.execute("UPDATE responses SET body_state='complete' WHERE response_id=1")
    con.execute("INSERT INTO urls(url_id,url) VALUES(2,'https://example.test/other')")
    con.execute("UPDATE documents SET url_id=2 WHERE document_id=?", (document_id,))
    with pytest.raises(ScanError, match="lineage"):
        read_document(con, document_id, max_decoded_bytes=100)

    con.execute("DELETE FROM documents")
    other_sha = _body(con, b"other")
    _document(con, other_sha)
    with pytest.raises(ScanError, match="lineage"):
        read_document(con, document_id, max_decoded_bytes=100)

    con.execute("DELETE FROM documents")
    _document(
        con,
        sha,
        body_sha256=None,
        source_response_id=None,
        fidelity="unavailable",
        body_state="unavailable",
        body_reason="not_in_corpus",
        decoder_source="not_applicable",
        decoder_charset="unknown",
        decoder_errors="not_applicable",
    )
    with pytest.raises(ScanError, match="unavailable: unavailable/not_in_corpus"):
        read_document(con, 1, max_decoded_bytes=100)


def test_read_document_handles_legacy_reencoded_text_and_rendered_dom_separately():
    con = _con()
    legacy_sha = _body(con, b"legacy text")
    _response(con, legacy_sha)
    con.execute("UPDATE responses SET body_fidelity='reencoded_text' WHERE response_id=1")
    _document(
        con,
        legacy_sha,
        fidelity="reencoded_text",
        decoder_source="legacy_unknown",
        decoder_charset="unknown",
        decoder_errors="unknown",
    )
    assert read_document(con, 1, max_decoded_bytes=100) == "legacy text"

    con.execute("DELETE FROM documents")
    dom_sha = _body(con, b"<main>DOM</main>")
    _document(
        con,
        dom_sha,
        source_response_id=None,
        representation="rendered",
        fidelity="serialized_dom",
        decoder_source="renderer_utf8",
        decoder_charset="utf-8",
        decoder_errors="not_applicable",
        renderer_json='{"engine":"test","engine_version":"1","settings":{},"flattened_iframes":false,"capture_limitations":[],"navigation_url_id":1,"final_url_id":1,"navigation_transform":"direct"}',
    )
    assert read_document(con, 1, max_decoded_bytes=100) == "<main>DOM</main>"


def test_legacy_fragment_reads_the_explicit_navigation_response():
    con = _con()
    con.execute(
        "INSERT INTO urls(url_id,url) VALUES(2,'https://example.test/?_escaped_fragment_=x')"
    )
    sha = _body(con, b"legacy")
    _response(con, sha)
    con.execute("UPDATE responses SET request_url_id=2 WHERE response_id=1")
    _document(
        con,
        sha,
        representation="legacy_fragment",
        renderer_json='{"engine":"legacy","engine_version":"1","settings":{},"flattened_iframes":false,"capture_limitations":[],"navigation_url_id":2,"final_url_id":2,"navigation_transform":"legacy_escaped_fragment"}',
    )
    with pytest.raises(ScanError, match="reproduce"):
        read_document(con, 1, max_decoded_bytes=100)
    con.execute("UPDATE urls SET url='https://example.test/?_escaped_fragment_=' WHERE url_id=2")
    assert read_document(con, 1, max_decoded_bytes=100) == "legacy"


def test_binary_codec_is_not_a_charset():
    text, decoder = decode_entity(b"\xffhello", "text/html; charset=base64_codec")
    assert text == "\ufffdhello"
    assert decoder["decoder_source"] == "utf8_fallback"


def test_http_entity_cannot_be_relabelled_as_a_rendered_dom():
    con = _con()
    sha = _body(con, b"HTTP entity")
    _response(con, sha)
    _document(
        con,
        sha,
        representation="rendered",
        renderer_json='{"engine":"test","engine_version":"1","settings":{},"flattened_iframes":false,"capture_limitations":[],"navigation_url_id":1,"final_url_id":1,"navigation_transform":"direct"}',
    )
    with pytest.raises(ScanError, match=r"representation.*fidelity"):
        read_document(con, 1, max_decoded_bytes=100)


def test_document_window_counts_characters_and_keeps_multibyte_whole():
    con = _con()
    raw = "café 🙂\nsecond line\n".encode()
    sha = _body(con, raw)
    _response(con, sha)
    con.execute("UPDATE responses SET content_type='text/html; charset=utf-8' WHERE response_id=1")
    _document(con, sha, decoder_charset="utf-8")
    text = raw.decode()

    window = read_document_window(con, 1, offset=3, length=3, max_decoded_bytes=100)

    assert window["text"] == "é 🙂"
    assert window["offset"] == 3 and window["length"] == 3
    assert window["total_chars"] == len(text)
    assert window["truncated"] is True
    assert window["lines"] == 2
    assert window["encoding"] == "utf-8"
    assert window["sha256"] == hashlib.sha256(text.encode()).hexdigest()
    assert window["raw_body_sha256"] == hashlib.sha256(raw).hexdigest()


@pytest.mark.parametrize("text", ["", "one\n", "one\ntwo", "\r\n\n", "a\v b\u2028c\x1cd"])
def test_line_count_matches_splitlines_without_materializing_lines(text):
    assert _line_count(text) == len(text.splitlines())


def test_document_window_full_read_and_empty_body():
    con = _con()
    sha = _body(con, b"caf\xe9")
    _response(con, sha)
    _document(con, sha)
    full = read_document_window(con, 1, length=1000, max_decoded_bytes=100)
    assert full["text"] == "café"
    assert full["truncated"] is False
    assert full["encoding"] == "iso8859-1"

    empty_sha = _body(con, b"")
    con.execute("UPDATE responses SET body_sha256=? WHERE response_id=1", (empty_sha,))
    con.execute("UPDATE documents SET body_sha256=? WHERE document_id=1", (empty_sha,))
    empty = read_document_window(con, 1, max_decoded_bytes=100)
    assert empty["text"] == ""
    assert empty["total_chars"] == 0
    assert empty["lines"] == 0
    assert empty["truncated"] is False


def test_document_window_rejects_invalid_bounds_and_respects_byte_limit():
    con = _con()
    sha = _body(con, b"caf\xe9")
    _response(con, sha)
    _document(con, sha)
    for kwargs in (
        {"offset": -1},
        {"offset": True},
        {"length": 0},
        {"length": MAX_WINDOW_CHARS + 1},
    ):
        with pytest.raises(ScanError, match="window"):
            read_document_window(con, 1, max_decoded_bytes=100, **kwargs)
    with pytest.raises(ScanError, match="decoded byte limit"):
        read_document_window(con, 1, max_decoded_bytes=2)


def test_page_document_content_returns_complete_text_and_provenance_under_cap():
    con = _con()
    raw = b"caf\xe9"
    sha = _body(con, raw)
    _response(con, sha)
    _document(con, sha)

    result = read_page_document_content(
        con,
        "https://example.test/",
        max_decoded_bytes=4,
    )

    assert result["state"] == "available"
    assert result["document_id"] == 1
    assert result["representation"] == "static"
    assert result["content_type"] == "text/html; charset=iso-8859-1"
    assert result["raw_body_sha256"] == sha
    assert result["sha256"] == hashlib.sha256("café".encode()).hexdigest()
    assert result["text"] == "café"
    with pytest.raises(ScanError, match="decoded byte limit"):
        read_page_document_content(
            con,
            "https://example.test/",
            max_decoded_bytes=3,
        )

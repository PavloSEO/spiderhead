"""Bounded retained-body reads are available through the shared interfaces."""

from __future__ import annotations

import hashlib
import json
import sqlite3

from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.storage.bodies import read_page_document_content
from tests.test_scan_body_diff_integration import (
    PAGE_URL,
    _renderer,
    _static_and_rendered_scan,
)


def test_body_read_returns_a_window_and_both_hash_identities(tmp_path):
    path = tmp_path / "scan.sqlite"
    raw = b"<html>caf\xc3\xa9</html>"
    _static_and_rendered_scan(
        path,
        static=raw,
        diagnostic=b"<html>diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )

    result = handlers.scan_body_read(
        str(path), PAGE_URL, offset=6, length=4, max_decoded_bytes=1024
    )

    assert result["state"] == "available"
    assert result["text"] == "café"
    assert result["raw_body_sha256"] == hashlib.sha256(raw).hexdigest()
    assert result["sha256"] == hashlib.sha256(raw.decode().encode()).hexdigest()


def test_selected_page_excludes_unbound_diagnostic_variant(tmp_path):
    path = tmp_path / "scan.sqlite"
    raw = b"<html>selected page</html>"
    _static_and_rendered_scan(
        path,
        static=raw,
        diagnostic=b"<html>diagnostic latest</html>",
        renderer=_renderer(PAGE_URL),
    )
    with sqlite3.connect(path) as scan:
        documents = scan.execute(
            "SELECT document_id,source_response_id FROM documents "
            "WHERE representation='static' ORDER BY document_id"
        ).fetchall()
        selected_id, selected_response = documents[0]
        diagnostic_response = documents[-1][1]
        url_id = scan.execute("SELECT url_id FROM urls WHERE url=?", (PAGE_URL,)).fetchone()[0]
        scan.execute(
            "UPDATE pages SET document_id=?,representation='static' WHERE url_id=?",
            (selected_id, url_id),
        )
        scan.execute("DELETE FROM context_items WHERE kind='resource_inventory'")
        scan.execute(
            "UPDATE responses SET variant_key='selected-v1' WHERE response_id=?",
            (selected_response,),
        )
        scan.execute(
            "UPDATE responses SET variant_key='diagnostic-v2' WHERE response_id=?",
            (diagnostic_response,),
        )
        result = read_page_document_content(scan, PAGE_URL, max_decoded_bytes=1024)
    assert result["state"] == "available"
    assert result["document_id"] == selected_id
    assert result["text"] == raw.decode()


def test_body_read_cli_and_mcp_share_the_bounded_reader(tmp_path, capsys):
    from seohead import cli

    path = tmp_path / "scan.sqlite"
    _static_and_rendered_scan(
        path,
        static=b"<html>hello</html>",
        diagnostic=b"<html>diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )
    assert (
        cli.main(
            [
                "scan-body-read",
                "--scan",
                str(path),
                "--url",
                PAGE_URL,
                "--offset",
                "6",
                "--length",
                "5",
            ]
        )
        == 0
    )
    command_result = json.loads(capsys.readouterr().out)
    tool = build_server()._tool_manager.get_tool("seo_scan_body_read")
    mcp_result = tool.fn(input_path=str(path), url=PAGE_URL, offset=6, length=5)
    assert command_result == mcp_result


def test_url_detail_exposes_bounded_source_html_and_raw_hash(tmp_path):
    path = tmp_path / "scan.sqlite"
    raw = b"<html>source page</html>"
    _static_and_rendered_scan(
        path,
        static=raw,
        diagnostic=b"<html>diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )

    result = handlers.scan_url_detail(str(path), PAGE_URL, body_offset=6, body_length=6)

    assert result["source_html"]["state"] == "available"
    assert result["source_html"]["text"] == "source"
    assert result["source_html"]["raw_body_sha256"] == hashlib.sha256(raw).hexdigest()

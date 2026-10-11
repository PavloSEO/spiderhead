"""Bounded retained-body reads are available through the shared interfaces."""

from __future__ import annotations

import hashlib
import json

from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
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


def test_body_read_cli_and_mcp_share_the_bounded_reader(tmp_path, capsys):
    from seohead import cli

    path = tmp_path / "scan.sqlite"
    _static_and_rendered_scan(
        path,
        static=b"<html>hello</html>",
        diagnostic=b"<html>diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )
    assert cli.main(
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
    ) == 0
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

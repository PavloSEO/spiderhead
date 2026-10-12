"""Header analysis reads retained redacted evidence and never refetches a URL."""

from __future__ import annotations

import json

from seohead import cli
from seohead.crawl.capture import unavailable_transport_phases
from seohead.crawl.settings import fingerprint, load
from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.storage.headers_analyze import analyze
from seohead.storage.native_scan import NativeScan
from tests.test_native_capture import _event
from tests.test_scan_native import _metadata, _record, _runtime

URL = "https://example.test/"


def _scan(path):
    metadata = _metadata()
    metadata["config"] = load(overrides={"storage.format_version": "scan.v2"})
    metadata["config_fingerprint"] = fingerprint(metadata["config"])
    phases = unavailable_transport_phases("phase not observed")
    phases["connect"] = {"seconds": 0.01, "state": "measured", "reason": ""}
    phases["ttfb"] = {"seconds": 0.04, "state": "measured", "reason": ""}
    with NativeScan.create(path, format_version="scan.v2", **metadata) as scan:
        scan.enqueue([(URL, 0)])
        lease = scan.claim(1)[0]
        event = _event(
            URL,
            b"<html><title>Home</title></html>",
            response_headers=(
                ("content-type", "text/html; charset=utf-8"),
                ("strict-transport-security", "max-age=31536000"),
                ("cache-control", "public, max-age=600"),
            ),
            effective_headers=(
                ("content-type", "text/html; charset=utf-8"),
                ("strict-transport-security", "max-age=31536000"),
                ("cache-control", "public, max-age=600"),
            ),
            transport_phases=phases,
        )
        scan.commit_page(
            lease,
            _record(URL) | {"crawl_depth": 0},
            captures=[event],
            runtime=_runtime(),
        )
        scan.finish_capture()
    return path


def test_headers_analyze_reads_redacted_headers_and_saved_phases(tmp_path):
    path = _scan(tmp_path / "headers.sqlite")

    result = analyze(str(path), url=URL)

    assert result["ok"] is True
    assert result["summary"]["headers_measured"] == 1
    assert result["items"][0]["headers"]["cache-control"] == "public, max-age=600"
    assert result["items"][0]["transport"]["connect"]["seconds"] == 0.01
    assert result["items"][0]["transport"]["dns"]["state"] == "unavailable"
    assert "MISSING_HSTS" not in {item["check"] for item in result["items"][0]["findings"]}
    query = handlers.scan_url_query(
        input_path=str(path),
        columns=["url", "dns_seconds", "dns_state", "connect_seconds", "connect_state"],
    )
    assert query["rows"][0]["dns_seconds"] is None
    assert query["rows"][0]["dns_state"] == "unavailable"
    assert query["rows"][0]["connect_seconds"] == 0.01


def test_headers_analyze_is_registered_for_cli_and_mcp(tmp_path, capsys):
    path = _scan(tmp_path / "headers.sqlite")

    code = cli.main(["headers-analyze", "--scan", str(path), "--url", URL])
    output = json.loads(capsys.readouterr().out)
    assert code == 0 and output["items"][0]["url"] == URL
    tool = build_server()._tool_manager.get_tool("seo_headers_analyze")
    assert "input_path" in tool.parameters["properties"]
    assert handlers.headers_analyze(str(path), url=URL)["items"][0]["url"] == URL

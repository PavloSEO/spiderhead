"""Native workflows exercised unchanged on Windows, macOS and Linux."""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import ClassVar

import pytest

from seohead import __version__
from seohead.crawl.settings import load
from seohead.crawl.sqlite_adapter import crawl_to_scan
from seohead.mcp.reanalysis_handlers import reanalyze_scan
from seohead.projects.coverage import initialize_coverage
from seohead.projects.workspace import create_project, project_status
from seohead.storage import open_scan
from seohead.storage.history import snapshot_scan
from seohead.storage.retry import requeue_scan


class Response:
    status_code = 200
    headers: ClassVar = {"content-type": "text/html; charset=utf-8"}
    content = b"<html><head><title>Portable page</title></head><body><main>Saved portable evidence.</main></body></html>"
    text = content.decode()


def test_project_and_cli_support_unicode_and_spaces(tmp_path):
    root = tmp_path / "\u041f\u0440\u043e\u0435\u043a\u0442 with spaces"
    create_project(root, "https://example.test/")
    initialize_coverage(root)
    assert project_status(root)["checklist"]["counts"]["total"] > 0
    process = subprocess.run(
        [
            sys.executable,
            "-m",
            "seohead",
            "project-status",
            "--input",
            json.dumps({"directory": str(root)}),
        ],
        capture_output=True,
        encoding="utf-8",
        timeout=30,
    )
    assert process.returncode == 0, process.stderr
    assert json.loads(process.stdout)["ok"]


@pytest.mark.parametrize("version", ["scan.v1", "scan.v2"])
def test_saved_scan_roundtrip_and_retry_on_real_platform(tmp_path, version):
    root = tmp_path / "\u0414\u0430\u043d\u043d\u044b\u0435 with spaces"
    root.mkdir()
    path, backup, derived = root / "scan.sqlite", root / "backup.sqlite", root / "derived.sqlite"
    settings = load(
        overrides={
            "storage.format_version": version,
            "robots.policy": "ignore",
            "limits.max_urls": 2,
            "speed.min_delay_seconds": 0,
        }
    )
    kwargs = dict(
        scan_out=str(path),
        settings=settings,
        producer_version=__version__,
        producer_revision="a" * 40,
        runtime_versions={
            "python": "test",
            "sqlite": "test",
            "httpx": "test",
            "lxml": "test",
            "beautifulsoup4": "test",
        },
        fetcher=lambda _url: Response(),
        sleeper=lambda _: None,
    )
    crawl_to_scan("https://example.test/", **kwargs)
    con = open_scan(path, require_audit=False)
    try:
        assert con.execute("SELECT COUNT(*) FROM pages").fetchone()[0] == 1
    finally:
        con.close()
    snapshot_scan(path, backup)
    con = open_scan(backup, require_audit=False)
    con.close()
    requeue_scan(path, where="status_code = 200", backup_path=root / "before-retry.sqlite")
    # Explicit retry upgrades v1; resume reads the persisted settings, not a new config.
    crawl_to_scan("https://example.test/", **kwargs)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    reanalyze_scan(str(path), str(derived), producer_build="b" * 40)
    con = open_scan(derived)
    con.close()
    assert hashlib.sha256(path.read_bytes()).hexdigest() == digest


def test_real_stdio_mcp_starts_and_lists_router_tools():
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client

    async def run():
        params = StdioServerParameters(
            command=sys.executable,
            args=["-m", "seohead", "mcp", "--profile", "router"],
            env={**os.environ},
            cwd=str(Path.cwd()),
        )
        async with (
            stdio_client(params) as (reader, writer),
            ClientSession(reader, writer) as session,
        ):
            await session.initialize()
            names = {tool.name for tool in (await session.list_tools()).tools}
            assert names == {
                "seo_audit_workflow",
                "seo_doctor",
                "seo_inspect_url",
                "seo_tool_catalog",
            }
            result = await session.call_tool("seo_tool_catalog", {"query": "scan", "limit": 2})
            assert not result.isError

    asyncio.run(run())


def test_parser_supports_older_htmlparser_constructor(monkeypatch):
    from html.parser import HTMLParser

    from seohead.checks.parser import document_base_url, invalid_head_elements

    original = HTMLParser.__init__

    def legacy_init(self, *, convert_charrefs=True):
        original(self, convert_charrefs=convert_charrefs)

    monkeypatch.setattr(HTMLParser, "__init__", legacy_init)
    html = '<head><noscript><base href="https://ignored.test/"><div>hidden</div></noscript><title><p>literal</p></title><base href="/assets/"><div>visible</div></head>'
    assert invalid_head_elements(html) == ["div"]
    assert document_base_url(html, "https://example.test/") == "https://example.test/assets/"

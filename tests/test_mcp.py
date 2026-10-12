"""MCP server integration test over real stdio.

Skipped automatically when the optional ``mcp`` SDK isn't installed.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys

import pytest

pytest.importorskip("mcp")

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from seohead.mcp.mcp_server import build_server

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def test_all_mcp_tools_have_structured_schemas_and_safety_annotations():
    """Keep MCP clients informed about output shape, side effects, and network use."""
    tools = {tool.name: tool for tool in build_server()._tool_manager.list_tools()}

    from seohead.cli import COMMANDS
    from seohead.mcp.tool_reference import load_semantics_tools, load_sf_tools

    assert len(tools) == len(COMMANDS) + len(load_sf_tools()) + len(load_semantics_tools())
    assert all(tool.fn_metadata.output_schema for tool in tools.values())
    assert all(tool.annotations is not None for tool in tools.values())

    optimizer = tools["seo_images_optimize"].annotations
    assert optimizer.destructiveHint is True
    assert optimizer.readOnlyHint is False
    # Scan-backed optimization downloads the selected page images before writing;
    # the annotation covers that supported network-capable mode too.
    assert optimizer.openWorldHint is True

    # The only tool that writes to somebody else's service (#97). A caller decides
    # whether to ask a person first from these hints, so "submits URLs to four search
    # engines" must never read as a harmless read-only call. Idempotent because
    # resubmitting a URL is defined by the protocol to be safe; not destructive
    # because nothing is removed or overwritten.
    submit = tools["seo_indexnow_submit"].annotations
    assert submit.readOnlyHint is False
    assert submit.openWorldHint is True
    assert submit.idempotentHint is True
    assert submit.destructiveHint is False

    live_fetch = tools["seo_parse"].annotations
    assert live_fetch.readOnlyHint is True
    assert live_fetch.openWorldHint is True

    paid = tools["seo_google_keywords"].annotations
    assert paid.readOnlyHint is False
    assert paid.openWorldHint is True

    local_report = tools["seo_spend_report"].annotations
    assert local_report.readOnlyHint is True
    assert local_report.openWorldHint is False

    crawl = tools["sf_audit_run"].annotations
    assert crawl.readOnlyHint is False
    assert crawl.destructiveHint is False
    assert crawl.openWorldHint is True

    tasks = tools["sf_audit_tasks"].annotations
    assert tasks.readOnlyHint is False
    assert tasks.destructiveHint is False
    assert tasks.openWorldHint is False

    snippet_edits = tools["sf_snippet_edits"].annotations
    assert snippet_edits.readOnlyHint is False
    assert snippet_edits.destructiveHint is False
    assert snippet_edits.openWorldHint is False


def test_report_build_accepts_an_audit_document_or_json_path(monkeypatch):
    """The MCP schema must expose both input forms the shared handler supports (#247)."""
    received = []

    def fake_report_build(**kwargs):
        received.append(kwargs["audit"])
        return {"ok": True}

    monkeypatch.setattr("seohead.mcp.handlers.report_build", fake_report_build)
    tool = next(
        tool
        for tool in build_server()._tool_manager.list_tools()
        if tool.name == "seo_report_build"
    )

    assert asyncio.run(tool.run({"audit": {"synthetic": True}})) == {"ok": True}
    assert asyncio.run(tool.run({"audit": "synthetic-audit.json"})) == {"ok": True}
    assert received == [{"synthetic": True}, "synthetic-audit.json"]


async def _call_over_stdio(tool: str, arguments: dict) -> object:
    params = StdioServerParameters(
        command=sys.executable,
        args=["-m", "seohead.mcp.mcp_server"],
        cwd=ROOT,
        # The SDK's default child environment drops SEOHEAD_CONFIG_DIR, so the server
        # would read the developer's real shared MCP state instead of the isolated one.
        env=dict(os.environ),
    )
    async with (
        stdio_client(params) as (read, write),
        ClientSession(read, write) as session,
    ):
        await session.initialize()
        return await session.call_tool(tool, arguments)


def test_a_handler_reported_failure_sets_iserror():
    """A tool whose handler returns ``ok: false`` (issue #155) must be distinguishable from
    success without the client having to inspect the payload — the same distinction the CLI
    makes with a non-zero exit (docs/USAGE.md)."""
    result = asyncio.run(_call_over_stdio("seo_ai_bots_check", {"url": "not a url"}))
    assert result.isError is True
    assert "could not be resolved" in result.content[0].text


def test_a_clean_result_does_not_set_iserror():
    result = asyncio.run(_call_over_stdio("seo_spend_report", {}))
    assert result.isError is False


def _payload(r):
    sc = getattr(r, "structuredContent", None)
    if isinstance(sc, dict):
        return sc["result"] if set(sc.keys()) == {"result"} else sc
    return json.loads(r.content[0].text)


async def _drive(exports_dir: str, out_dir: str) -> dict:
    params = StdioServerParameters(
        command=sys.executable,
        args=["-m", "seohead.mcp.mcp_server"],
        cwd=ROOT,
        # The SDK's default child environment drops SEOHEAD_CONFIG_DIR, so the server
        # would read the developer's real shared MCP state instead of the isolated one.
        env=dict(os.environ),
    )
    async with (
        stdio_client(params) as (read, write),
        ClientSession(read, write) as session,
    ):
        await session.initialize()
        tools = {t.name for t in (await session.list_tools()).tools}

        le = _payload(await session.call_tool("sf_list_exports", {"exports_dir": exports_dir}))
        run = _payload(
            await session.call_tool(
                "sf_audit_run",
                {"mode": "parse-exports", "input": exports_dir, "out": out_dir},
            )
        )
        summ = _payload(
            await session.call_tool("sf_audit_summary", {"json_path": run["json_path"]})
        )
        issues = _payload(
            await session.call_tool(
                "sf_audit_issues",
                {"json_path": run["json_path"], "check": "BROKEN_INTERNAL_LINK", "limit": 5},
            )
        )
        backlog = _payload(
            await session.call_tool(
                "sf_audit_tasks",
                {"json_path": run["json_path"], "out": os.path.join(out_dir, "tasks")},
            )
        )
        return {
            "tools": tools,
            "list_exports": le,
            "run": run,
            "summary": summ,
            "issues": issues,
            "tasks": backlog,
        }


def test_mcp_all_tools(tmp_path, exports_dir):
    out = tmp_path / "out"

    res = asyncio.run(_drive(exports_dir, str(out)))

    assert {
        "sf_audit_run",
        "sf_audit_summary",
        "sf_audit_issues",
        "sf_list_exports",
        "sf_audit_tasks",
    } <= res["tools"]
    # One connector exposes both crawl-audit and live-analysis tools.
    assert {"seo_parse", "seo_robots_check", "seo_headers_check"} <= res["tools"]
    assert "internal_all" in res["list_exports"]["found"]
    assert os.path.isfile(res["run"]["json_path"])
    assert os.path.isfile(res["run"]["md_path"])
    assert res["summary"]["by_severity"]["critical"] >= 1
    assert res["issues"][0]["check"] == "BROKEN_INTERNAL_LINK"
    assert len(res["issues"][0]["locations"]) == 2
    assert res["tasks"]["summary"]["tasks_total"] >= 1
    assert os.path.isfile(res["tasks"]["tasks_md"])


def test_mcp_snippet_csv_exports_review_tasks(tmp_path):
    output = tmp_path / "snippet-tasks.json"
    tool = next(
        tool
        for tool in build_server()._tool_manager.list_tools()
        if tool.name == "sf_snippet_edits"
    )
    result = asyncio.run(
        tool.run(
            {
                "csv_text": "url,title,description\nhttps://example.test/p,New title,\n",
                "out": str(output),
            }
        )
    )
    assert result["summary"] == {"tasks": 1, "rejected_rows": 0}
    saved = json.loads(output.read_text(encoding="utf-8"))
    assert saved["tasks"][0]["target_url"] == "https://example.test/p"
    assert saved["tasks"][0]["state"] == "candidate"


def test_crawl_site_omitted_overrides_forward_none(monkeypatch):
    """Issue #327: a config-only MCP call must not shadow config values with the wrapper's
    own concrete defaults -- omitted overrides have to reach the handler as ``None``, the
    same shape the CLI forwards for an unset flag, so ``crawl.settings.load`` can fall
    through to config/environment/defaults instead of being overridden silently."""
    received = []

    def fake_crawl_site(**kwargs):
        received.append(kwargs)
        return {"ok": True}

    monkeypatch.setattr("seohead.mcp.handlers.crawl_site", fake_crawl_site)
    tool = next(
        tool for tool in build_server()._tool_manager.list_tools() if tool.name == "seo_crawl_site"
    )

    asyncio.run(tool.run({"url": "https://example.test/", "config": "crawl.json"}))

    forwarded = received[0]
    for key in ("max_urls", "max_depth", "min_delay", "robots", "concurrency"):
        assert forwarded[key] is None, f"{key} must default to None, not a concrete override"


def test_crawl_site_explicit_override_changes_only_that_setting(monkeypatch):
    """Positive control for #327: an explicit override still reaches the handler, and
    only that one setting -- the neighbouring overrides stay None."""
    received = []

    def fake_crawl_site(**kwargs):
        received.append(kwargs)
        return {"ok": True}

    monkeypatch.setattr("seohead.mcp.handlers.crawl_site", fake_crawl_site)
    tool = next(
        tool for tool in build_server()._tool_manager.list_tools() if tool.name == "seo_crawl_site"
    )

    asyncio.run(tool.run({"url": "https://example.test/", "max_urls": 2}))

    forwarded = received[0]
    assert forwarded["max_urls"] == 2
    for key in ("max_depth", "min_delay", "robots", "concurrency"):
        assert forwarded[key] is None


def test_compare_crawls_mcp_forwards_declared_correspondence(monkeypatch):
    received = []
    monkeypatch.setattr(
        "seohead.mcp.handlers.compare_crawls",
        lambda **kwargs: received.append(kwargs) or {"ok": True},
    )
    tool = build_server()._tool_manager.get_tool("seo_compare_crawls")
    correspondence = {
        "schema_version": "url-correspondence.v1",
        "origin_map": {},
        "pairs": [],
    }

    asyncio.run(
        tool.run(
            {
                "before": {"pages": [], "issues": []},
                "after": {"pages": [], "issues": []},
                "correspondence": correspondence,
                "out_dir": "compare-output",
                "compression": "gzip",
            }
        )
    )

    assert len(received) == 1
    forwarded = received[0]
    assert {
        key: forwarded[key]
        for key in (
            "before",
            "after",
            "force",
            "correspondence",
            "out_dir",
            "compression",
            "resume",
        )
    } == {
        "before": {"pages": [], "issues": []},
        "after": {"pages": [], "issues": []},
        "force": False,
        "correspondence": correspondence,
        "out_dir": "compare-output",
        "compression": "gzip",
        "resume": False,
    }
    assert callable(forwarded["progress"])
    assert hasattr(forwarded["cancel_event"], "set")
    assert hasattr(forwarded["cancel_event"], "is_set")


def test_crawl_site_forwards_the_rendering_browser_overrides(monkeypatch):
    """#744: MCP and CLI share one handler, so the engine/viewport/concurrency
    keys must arrive as the same dotted-path overrides a --set run builds --
    no separate MCP-side setting surface."""
    from seohead.crawl import settings as crawl_config

    received = []

    def fake_crawl_site(**kwargs):
        received.append(kwargs)
        return {"ok": True}

    monkeypatch.setattr("seohead.mcp.handlers.crawl_site", fake_crawl_site)
    tool = next(
        tool for tool in build_server()._tool_manager.list_tools() if tool.name == "seo_crawl_site"
    )
    overrides = {
        "rendering.browser.engine": "webkit",
        "rendering.browser.viewport_width": 1440,
        "rendering.browser.viewport_height": 900,
        "rendering.browser.page_concurrency": 2,
    }

    asyncio.run(tool.run({"url": "https://example.test/", "overrides": overrides}))

    forwarded = received[0]
    assert forwarded["overrides"] == overrides
    browser = crawl_config.load(overrides=forwarded["overrides"])["rendering"]["browser"]
    assert browser["engine"] == "webkit"
    assert (browser["viewport_width"], browser["viewport_height"]) == (1440, 900)
    assert browser["page_concurrency"] == 2


def test_crawl_describe_settings_exposes_file_type_filter_paths():
    tool = build_server()._tool_manager.get_tool("seo_crawl_describe_settings")

    result = asyncio.run(tool.run({}))

    rows = {row["path"]: row for row in result["settings"]}
    for path in (
        "scope.include_extensions",
        "scope.exclude_extensions",
        "scope.include_media_types",
        "scope.exclude_media_types",
    ):
        assert rows[path]["results_affecting"] is True
        assert rows[path]["description"]


def test_render_check_mcp_forwards_explicit_identity(monkeypatch):
    """#670: the local MCP surface cannot strand the mobile identity in the CLI."""
    received = []
    monkeypatch.setattr(
        "seohead.mcp.handlers.render_check",
        lambda **kwargs: received.append(kwargs) or {"ok": True},
    )
    tool = build_server()._tool_manager.get_tool("seo_render_check")

    assert asyncio.run(
        tool.run(
            {
                "url": "https://example.test/",
                "viewport": "mobile",
                "wait": "load",
                "user_agent": "Example/1.0",
            }
        )
    ) == {"ok": True}
    assert received == [
        {
            "url": "https://example.test/",
            "viewport": "mobile",
            "wait": "load",
            "user_agent": "Example/1.0",
        }
    ]

"""Bounded catalogue paging and URL-check batch behavior."""

import pytest

from seohead.mcp import handlers
from seohead.mcp.tool_reference import Argument, ToolSpec


def _tool(name: str, category: str = "content", project_aware: bool = False) -> ToolSpec:
    return ToolSpec(
        name=name,
        command=name.removeprefix("seo_"),
        summary="Synthetic check.",
        notes="",
        arguments=(Argument("url", "str", "required"),),
        network=True,
        writes=False,
        destructive=False,
        idempotent=True,
        paid=False,
        category=category,
        project_aware=project_aware,
    )


def test_tool_catalog_pages_after_filtering(monkeypatch):
    import seohead.mcp.tool_reference as reference

    monkeypatch.setattr(reference, "load_seo_tools", lambda: [_tool("seo_a"), _tool("seo_b")])
    monkeypatch.setattr(reference, "load_sf_tools", lambda: [])
    monkeypatch.setattr(reference, "load_semantics_tools", lambda: [])
    page = handlers.tool_catalog(limit=1, offset=1)
    assert page["total"] == 2
    assert [item["name"] for item in page["items"]] == ["seo_b"]
    assert page["offset"] == 1 and page["has_more"] is False


@pytest.mark.parametrize("limit,offset", [(0, 0), (51, 0), (1, -1), (True, 0)])
def test_tool_catalog_rejects_invalid_bounds(limit, offset):
    with pytest.raises(ValueError):
        handlers.tool_catalog(limit=limit, offset=offset)


def test_url_check_batch_keeps_one_failure_local_to_its_pair(monkeypatch):
    monkeypatch.setattr(handlers, "headers_check", lambda url, method="GET": {"url": url})

    def fail(url, user_agent="*", paths=None):
        raise OSError("temporary network failure")

    monkeypatch.setattr(handlers, "robots_check", fail)
    result = handlers.url_check_batch(
        ["https://example.test/a"], ["headers-check", "robots-check"]
    )
    assert result["count"] == 2
    assert result["results"][0]["result"] == {"url": "https://example.test/a"}
    assert result["results"][1]["error"] == "temporary network failure"
    assert result["ok"] is True and result["partial"] is True


def test_url_check_batch_rejects_oversized_or_unlisted_work_before_requests():
    with pytest.raises(ValueError, match="50 URL/check combinations"):
        handlers.url_check_batch(
            [f"https://example.test/{i}" for i in range(11)],
            ["parse", "robots-check", "headers-check", "schema-check", "links-check"],
        )
    with pytest.raises(ValueError, match="supported"):
        handlers.url_check_batch(["https://example.test/"], ["arbitrary-tool"])


def test_url_check_batch_reads_bounded_pages_from_a_retained_scan(monkeypatch):
    monkeypatch.setattr(
        handlers,
        "scan_url_query",
        lambda *args, **kwargs: {
            "ok": True,
            "columns": ["url"],
            "rows": [["https://example.test/page"]],
            "has_more": True,
        },
    )
    monkeypatch.setattr(handlers, "headers_check", lambda url, method="GET": {"ok": True})
    result = handlers.url_check_batch(scan="scan.sqlite", checks=["headers-check"], offset=40)
    assert result["source"] == "retained_scan"
    assert result["offset"] == 40 and result["has_more"] is True
    assert result["results"][0]["url"] == "https://example.test/page"

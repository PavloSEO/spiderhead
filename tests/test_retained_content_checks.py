"""Native selected-body DOM/duplicate checks, persisted and readable offline."""

from __future__ import annotations

import sqlite3
from unittest.mock import patch

import pytest

from seohead import __version__
from seohead.crawl.settings import load
from seohead.crawl.sqlite_adapter import crawl_to_scan
from seohead.sf.core import heuristics
from seohead.storage import ScanError, open_scan
from seohead.storage.bodies import read_page_document
from tests.test_scan_reanalysis_integration import (
    _Response,
    _runtime_versions,
    _save_native_audit,
)

PARAGRAPH = (
    "Technical crawlers discover indexable documents and inspect content, links, "
    "headings, metadata, and canonical URLs for an evidence based website audit. "
) * 8
BASE = "https://example.test"


def _native(tmp_path, *, body_mode="captured_entity_bytes", body_limit=None):
    paths = ("/a", "/a/", "/a?view=1", "/near", "/canonical", "/unique")
    texts = {
        "/a": PARAGRAPH,
        "/a/": PARAGRAPH,
        "/a?view=1": "Distinct astronomy observations of galaxies, telescopes and stars.",
        "/near": PARAGRAPH.replace("website", "site", 1),
        "/canonical": PARAGRAPH,
        "/unique": "Independent recipes for bread, flour, water, yeast and baking.",
    }
    links = "".join(f'<a href="{path}">{path}</a>' for path in paths)

    def fetch(url):
        if url.endswith("/robots.txt"):
            return _Response(b"User-agent: *\nAllow: /", "text/plain")
        path = url.removeprefix(BASE)
        canonical = f'<link rel="canonical" href="{BASE}/a">' if path == "/canonical" else ""
        html = (
            f"<html><head><title>{path}</title>{canonical}</head><body>"
            f"<nav>{links}</nav><main><div><p>{texts[path]}</p></div></main></body></html>"
        )
        return _Response(html.encode(), "text/html; charset=utf-8")

    overrides = {
        "speed.min_delay_seconds": 0,
        "limits.max_urls": 10,
        "resources.fetch": False,
        "storage.body_mode": body_mode,
    }
    if body_limit is not None:
        overrides["storage.max_body_bytes"] = body_limit
    settings = load(overrides=overrides)
    path = tmp_path / "source.sqlite"
    crawl_to_scan(
        BASE + "/a",
        scan_out=str(path),
        settings=settings,
        producer_version=__version__,
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=fetch,
        sleeper=lambda _seconds: None,
    )
    from seohead.crawl.sqlite_adapter import retained_start_gate
    from seohead.storage.native_scan import NativeScan

    # Body-disabled fixtures have no start-page HTML, so keep the helper's
    # rendering gate empty while exercising the retained audit checks.
    with (
        patch(
            "seohead.crawl.sqlite_adapter.retained_start_gate",
            side_effect=lambda scan, settings: retained_start_gate(scan, settings) or {},
        ),
        patch.object(NativeScan, "save_audit", NativeScan._save_audit_v2),
    ):
        _save_native_audit(path, settings, start_url=BASE + "/a")
    return path


def _audit(path):
    from seohead.storage import read_audit

    return read_audit(path)


def test_native_duplicates_are_persisted_queryable_and_exact_url_bound(tmp_path):
    from seohead.mcp.handlers import scan_url_query

    path = _native(tmp_path)
    audit = _audit(path)
    exact = [issue for issue in audit["issues"] if issue["check"] == "DUPLICATE_BY_HASH"]
    near = [issue for issue in audit["issues"] if issue["check"] == "NEAR_DUPLICATE"]
    assert {issue["target_url"] for issue in exact} == {BASE + "/a", BASE + "/a/"}
    assert BASE + "/near" in {issue["target_url"] for issue in near}
    assert not {BASE + "/unique", BASE + "/a?view=1", BASE + "/canonical"} & {
        issue["target_url"] for issue in exact + near
    }
    assert all(issue["details"]["status_code"] == 200 for issue in exact + near)
    assert all(issue["details"]["comparison_url"] != issue["target_url"] for issue in exact + near)
    query = scan_url_query(str(path), issue_check="duplicate_by_hash", columns=["url"])
    assert query["ok"], query
    # The current query surface does not ship a per-issue URL index. Do not
    # confuse its explicit unavailable result with no duplicate findings.
    assert query["state"] == "unavailable" and query["reason_code"] == "issue_index_missing"
    with open_scan(path) as con:
        assert PARAGRAPH in read_page_document(con, BASE + "/a/", max_decoded_bytes=5_000_000)
        assert "astronomy" in read_page_document(
            con, BASE + "/a?view=1", max_decoded_bytes=5_000_000
        )
        with pytest.raises(ScanError, match="not_in_corpus"):
            read_page_document(con, BASE + "/A/", max_decoded_bytes=5_000_000)


@pytest.mark.parametrize(
    "body_mode,limit,reason",
    [
        ("off", None, "omitted/not_enabled"),
        ("captured_entity_bytes", 100, "omitted/body_budget_exhausted"),
    ],
)
def test_native_unretained_bodies_report_real_reason(tmp_path, body_mode, limit, reason):
    path = _native(tmp_path, body_mode=body_mode, body_limit=limit)
    audit = _audit(path)
    skips = {item["id"]: item["reason"] for item in audit["run"]["checks_skipped"]}
    for check in ("DOM_TOO_DEEP", "DOM_TOO_MANY_NODES", "NEAR_DUPLICATE", "DUPLICATE_BY_HASH"):
        assert reason in skips[check], skips[check]


def test_retained_dom_metrics_use_body_lineage_and_reject_corruption(tmp_path):
    import pandas as pd

    from seohead.sf.config import load_config
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import LoadedExports

    path = _native(tmp_path)
    config = load_config(None)
    config["thresholds"].update(dom_depth_max=1, dom_nodes_max=2)
    exports = LoadedExports(
        {
            "internal_all": pd.DataFrame(
                [
                    {
                        "Address": BASE + path,
                        "Content Type": "text/html",
                        "Status Code": 200,
                        "Indexability": "Indexable",
                    }
                    for path in ("/a", "/a?view=1")
                ]
            )
        }
    )
    ctx = AuditContext(exports, config, disk_backed_pages=True)
    try:
        with sqlite3.connect(path) as con:
            ctx.scan_con = con
            heuristics.check_dom(ctx)
            assert len(ctx.issues) == 4
            assert all(page.metrics["dom_depth"] > 1 for page in ctx.pages)
            con.execute("UPDATE responses SET body_state='unavailable',body_reason='not_fetched'")
            with pytest.raises(ScanError, match="lineage disagrees"):
                read_page_document(con, BASE + "/a", max_decoded_bytes=5_000_000)
    finally:
        ctx.close()


@pytest.mark.parametrize("unparseable", ["", " \n\t", "<!-- no document element -->"])
def test_retained_dom_unparseable_body_is_omitted_without_aborting(tmp_path, unparseable):
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import load_exports
    from tests.test_near_duplicate_content import _page_html, _write

    valid_url, invalid_url = BASE + "/valid.html", BASE + "/invalid.html"
    exports, cfg = _write(
        tmp_path,
        [valid_url, invalid_url],
        {valid_url: _page_html("Readable retained content."), invalid_url: unparseable},
    )
    ctx = AuditContext(load_exports(exports), cfg)
    try:
        heuristics.check_dom(ctx)
        pages = {page.url: page for page in ctx.pages}
        assert pages[valid_url].metrics["dom_nodes"] > 0
        assert pages[invalid_url].metrics["dom_nodes"] is None
        assert pages[invalid_url].metrics["dom_depth"] is None
        assert not ctx.issues
        skipped = {item.id: item.reason for item in ctx.skipped}
        for check in ("DOM_TOO_DEEP", "DOM_TOO_MANY_NODES"):
            assert "matched only 1 of 2" in skipped[check]
            assert "DOM size not assessed for the rest" in skipped[check]
    finally:
        ctx.close()


def test_duplicate_budget_reports_partial_instead_of_clean(tmp_path, monkeypatch):
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import load_exports
    from seohead.storage import corpus_inputs
    from tests.test_near_duplicate_content import _PARAGRAPH, _page_html, _write

    urls = [BASE + f"/p{i}.html" for i in range(3)]
    exports, cfg = _write(tmp_path, urls, {url: _page_html(_PARAGRAPH) for url in urls})
    monkeypatch.setattr(corpus_inputs, "MAX_CORPUS_DOCUMENTS", 2)
    ctx = AuditContext(load_exports(exports), cfg)
    try:
        heuristics.check_content_duplication(ctx)
        assert len(ctx.issues) == 2
        assert all("2 of 3" in issue.details["html_coverage"] for issue in ctx.issues)
        assert all(
            "document limit exceeded" in issue.details["html_coverage"] for issue in ctx.issues
        )
    finally:
        ctx.close()


def test_exact_hashes_survive_near_comparison_budget_exhaustion(tmp_path, monkeypatch):
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import load_exports
    from seohead.storage import corpus_inputs
    from tests.test_near_duplicate_content import _PARAGRAPH, _page_html, _write

    urls = [BASE + f"/p{i}.html" for i in range(2)]
    exports, cfg = _write(tmp_path, urls, {url: _page_html(_PARAGRAPH) for url in urls})
    monkeypatch.setattr(corpus_inputs, "MAX_DUPLICATE_CANDIDATE_COMPARISONS", 0)
    ctx = AuditContext(load_exports(exports), cfg)
    try:
        heuristics.check_content_duplication(ctx)
        assert {
            issue.target_url for issue in ctx.issues if issue.check == "DUPLICATE_BY_HASH"
        } == set(urls)
        assert {item.id: item.reason for item in ctx.skipped}[
            "NEAR_DUPLICATE"
        ] == "duplicate candidate-comparison budget exceeded"
    finally:
        ctx.close()


def test_million_url_population_stops_body_reads_at_duplicate_document_budget(
    tmp_path, monkeypatch
):
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import load_exports
    from seohead.storage import corpus_inputs
    from tests.test_near_duplicate_content import _PARAGRAPH, _page_html, _write

    urls = [BASE + f"/p{i}.html" for i in range(3)]
    exports, cfg = _write(tmp_path, urls, {url: _page_html(_PARAGRAPH) for url in urls})
    ctx = AuditContext(load_exports(exports), cfg)
    pages = list(ctx.indexable_html_pages())

    class MillionPages:
        def __len__(self):
            return 1_000_000

        def __iter__(self):
            yield from pages
            for _ in range(1_000_000 - len(pages)):
                yield pages[-1]

    monkeypatch.setattr(ctx, "indexable_html_pages", lambda: MillionPages())
    monkeypatch.setattr(corpus_inputs, "MAX_CORPUS_DOCUMENTS", 2)
    reader = heuristics._stored_html_reader(ctx)
    reads = []
    monkeypatch.setattr(
        heuristics, "_stored_html_reader", lambda ctx: lambda url: reads.append(url) or reader(url)
    )
    try:
        heuristics.check_content_duplication(ctx)
        assert reads == urls[:2]
        assert all("2 of 1000000" in issue.details["html_coverage"] for issue in ctx.issues)
        assert all(
            "document limit exceeded (999998)" in issue.details["html_coverage"]
            for issue in ctx.issues
        )
    finally:
        ctx.close()


def test_disabled_body_checks_do_not_read_documents(tmp_path, monkeypatch):
    from seohead.sf.config import load_config
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import LoadedExports

    cfg = load_config(None)
    cfg["checks"] = {
        check: {"enabled": False}
        for check in (
            "DOM_TOO_DEEP",
            "DOM_TOO_MANY_NODES",
            "NEAR_DUPLICATE",
            "DUPLICATE_BY_HASH",
        )
    }
    ctx = AuditContext(LoadedExports(), cfg)
    monkeypatch.setattr(heuristics, "_stored_html_reader", lambda ctx: pytest.fail("body read"))
    try:
        heuristics.check_dom(ctx)
        heuristics.check_content_duplication(ctx)
        assert not ctx.issues and not ctx.skipped
    finally:
        ctx.close()

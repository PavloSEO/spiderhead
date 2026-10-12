"""click_depth in scan-url-query: shortest internal link path from the start page (synthetic graph only)."""

from __future__ import annotations

import pytest

from seohead.mcp import handlers
from seohead.storage.native_scan import NativeScan
from tests.test_scan_native import _metadata, _record, _runtime
from tests.test_scan_url_links import _edge

ROOT = "https://example.test/"
CAT = ROOT + "catalog/"
SUB = CAT + "sub/"
DEEP = SUB + "deep/"  # sitemap-seeded at crawl depth 0, but three clicks from the start page
SIDE = ROOT + "side"
ORPHAN = ROOT + "orphan"  # sitemap-seeded, linked from nowhere
OUT = "https://outside.test/x"
PAGES = (ROOT, CAT, SUB, DEEP, SIDE, ORPHAN)


def _scan(tmp_path, *, links_retained=True, format_version="scan.v1"):
    path = tmp_path / "click.sqlite"
    metadata = _metadata(**{"storage.format_version": format_version})
    with NativeScan.create(path, format_version=format_version, **metadata) as scan:
        scan.begin_collection(links_available=links_retained)
        # Every page is seeded at depth 0, the way a sitemap seeds them.
        scan.enqueue([(url, 0) for url in PAGES])
        edges = {
            ROOT: [_edge(ROOT, CAT), _edge(ROOT, SIDE), _edge(ROOT, OUT)],
            CAT: [_edge(CAT, SUB), _edge(CAT, ROOT)],
            SUB: [_edge(SUB, DEEP)],
            SIDE: [_edge(SIDE, DEEP), _edge(SIDE, SIDE)],  # DEEP also reachable in 2 clicks
        }
        for url in PAGES:
            lease = scan.claim(1)[0]
            record = _record(url) | {"crawl_depth": 0}
            runtime = _runtime() | {"max_depth_reached": 0}
            scan.commit_page(lease, record, links=edges.get(url, []), runtime=runtime)
        scan.finish_capture()
    return path


def _depths(path):
    got = handlers.scan_url_query(
        input_path=str(path), columns=["url", "crawl_depth", "click_depth"], limit=100
    )
    assert got["ok"] is True, got
    return {r["url"]: r["click_depth"] for r in got["rows"]}, {
        r["url"]: r["crawl_depth"] for r in got["rows"]
    }


def test_click_depth_is_the_link_path_while_crawl_depth_stays_frontier_depth(tmp_path):
    click, crawl = _depths(_scan(tmp_path))
    assert click == {ROOT: 0, CAT: 1, SIDE: 1, SUB: 2, DEEP: 2, ORPHAN: None}
    assert set(crawl.values()) == {0}


def test_filter_and_sort_use_click_depth(tmp_path):
    path = _scan(tmp_path)
    got = handlers.scan_url_query(
        input_path=str(path),
        columns=["url", "click_depth"],
        filters=[{"column": "click_depth", "op": "lte", "value": 1}],
        sort="click_depth",
        limit=100,
    )
    assert got["ok"] is True, got
    assert sorted(r["click_depth"] for r in got["rows"]) == [0, 1, 1]


def test_scan_v2_persists_an_indexed_click_depth_projection(tmp_path):
    path = _scan(tmp_path, format_version="scan.v2")
    import sqlite3

    con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        assert con.execute("SELECT COUNT(*) FROM scan_click_depth").fetchone()[0] == 5
        assert (
            con.execute(
                "SELECT depth FROM scan_click_depth JOIN urls USING(url_id) WHERE url=?", (DEEP,)
            ).fetchone()[0]
            == 2
        )
        assert con.execute(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='scan_click_depth_depth'"
        ).fetchone()
    finally:
        con.close()


def test_scan_without_retained_links_refuses_click_depth_and_serves_other_columns(tmp_path):
    path = _scan(tmp_path, links_retained=False)
    refused = handlers.scan_url_query(
        input_path=str(path), columns=["url", "click_depth"], limit=10
    )
    assert refused["ok"] is False
    assert refused["reason_code"] == "click_depth_unavailable"
    assert refused["state"] == "unavailable"
    plain = handlers.scan_url_query(input_path=str(path), columns=["url", "crawl_depth"], limit=10)
    assert plain["ok"] is True and len(plain["rows"]) == len(PAGES)


def test_default_columns_are_unchanged_for_click_depth(tmp_path):
    path = _scan(tmp_path)
    got = handlers.scan_url_query(input_path=str(path), limit=10)
    assert got["ok"] is True
    assert "click_depth" not in got["rows"][0]


@pytest.mark.parametrize("bad", [{"column": "click_depth", "op": "contains", "value": "1"}])
def test_click_depth_follows_the_int_operator_rules(tmp_path, bad):
    got = handlers.scan_url_query(input_path=str(_scan(tmp_path)), filters=[bad], limit=10)
    assert got["ok"] is False

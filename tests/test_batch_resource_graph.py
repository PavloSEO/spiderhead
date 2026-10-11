"""Offline regressions for scan.v2 declared-resource evidence."""

from __future__ import annotations

import base64
import hashlib
import json
import sqlite3
import struct

import pytest

from seohead.crawl.settings import load
from seohead.crawl.sqlite_adapter import crawl_to_scan
from seohead.storage import open_scan
from seohead.storage.native_scan import NativeScan
from seohead.storage.resource_graph import (
    _image_dimensions,
    capture,
    extract,
    invalidate_pages,
    read,
    store_document,
)
from seohead.storage.retry import requeue_scan
from tests.test_native_capture import _claim, _event
from tests.test_scan_native import _metadata, _record, _runtime


class _Response:
    def __init__(self, status_code: int, content: bytes, headers: dict[str, str]) -> None:
        self.status_code = status_code
        self.content = content
        self.text = content.decode("utf-8", "replace")
        self.headers = headers


def _v2_metadata(**overrides):
    return _metadata(
        **{
            "storage.format_version": "scan.v2",
            "resources.fetch": True,
            "speed.min_delay_seconds": 0,
            **overrides,
        }
    )


def _store_html(scan: NativeScan, html: str) -> int:
    lease = _claim(scan)
    scan.commit_page(
        lease,
        _record(lease.url),
        captures=[_event(lease.url, html.encode())],
        runtime=_runtime(),
    )
    document_id = scan.con.execute(
        "SELECT document_id FROM documents WHERE url_id=? AND representation='static'",
        (lease.url_id,),
    ).fetchone()[0]
    store_document(
        scan.con,
        page_url_id=lease.url_id,
        source_document_id=document_id,
        representation="static",
        html=html,
    )
    return lease.url_id


def test_integrity_is_per_declaration_not_collapsed_by_resource_url(tmp_path):
    path = tmp_path / "scan.sqlite"
    html = (
        '<script src="/shared.js" integrity="sha384-first"></script>'
        '<script src="/shared.js"></script>'
        '<link rel="stylesheet" href="/site.css" integrity="">'
    )
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, html)
        rows = list(
            scan.con.execute(
                "SELECT raw_url,integrity,integrity_state FROM resource_graph_occurrences "
                "ORDER BY ordinal"
            )
        )

    assert [tuple(row) for row in rows] == [
        ("/shared.js", "sha384-first", "declared"),
        ("/shared.js", None, "absent"),
        ("/site.css", "", "declared"),
    ]


def test_resource_graph_uses_the_document_base_for_direct_and_inline_css_urls():
    rows, omitted = extract(
        '<base href="https://cdn.example.test/assets/">'
        '<script src="app.js"></script><style>.hero{background:url(hero.png)}</style>',
        "https://example.test/current/page",
    )

    assert omitted == 0
    assert [row["resolved_url"] for row in rows] == [
        "https://cdn.example.test/assets/app.js",
        "https://cdn.example.test/assets/hero.png",
    ]


def test_intrinsic_dimensions_require_a_complete_safe_retained_entity(tmp_path):
    png = base64.b64decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAF/gL+Mhp0+wAAAABJRU5ErkJggg=="
    )
    malicious_header = (
        b"\x89PNG\r\n\x1a\n"
        + struct.pack(">I", 13)
        + b"IHDR"
        + struct.pack(">II", 100_000, 100_000)
        + b"\x08\x02\x00\x00\x00\x00\x00\x00\x00"
    )

    path = tmp_path / "image.sqlite"
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/pixel.png">')
        capture(
            scan,
            settings,
            fetcher=lambda _url: _Response(200, png, {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        row = scan.con.execute(
            "SELECT width,height,body_state FROM resource_graph_fetches"
        ).fetchone()

    assert tuple(row) == (1, 1, "complete")
    assert _image_dimensions(png[:20]) == (None, None)
    assert _image_dimensions(None) == (None, None)
    assert _image_dimensions(malicious_header) == (None, None)


def test_redirect_outside_scope_is_stored_as_excluded_without_following_target(tmp_path):
    path = tmp_path / "scan.sqlite"
    calls: list[str] = []
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})

    def fetcher(url: str):
        calls.append(url)
        return _Response(302, b"", {"location": "https://outside.test/image.png"})

    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/image.png">')
        totals = capture(scan, settings, fetcher=fetcher, wait=lambda: None, clock=lambda: 0.0)
        row = scan.con.execute(
            "SELECT state,reason,final_url FROM resource_graph_fetches"
        ).fetchone()

    assert calls == ["https://example.test/image.png"]
    assert totals == {"stored": 0, "fetched": 0, "excluded": 1, "failed": 0, "budget": 0}
    assert tuple(row) == ("excluded", "outside_host", "https://example.test/image.png")


def test_graph_request_budget_counts_a_redirect_hop_before_following_it(tmp_path):
    path = tmp_path / "scan.sqlite"
    calls: list[str] = []
    settings = load(
        overrides={
            "storage.format_version": "scan.v2",
            "resources.fetch": True,
            "resources.graph.max_requests": 1,
            "resources.graph.max_redirects": 2,
        }
    )

    def fetcher(url: str):
        calls.append(url)
        return _Response(302, b"hop", {"location": "/final.js"})

    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<script src="/redirect.js"></script>')
        totals = capture(scan, settings, fetcher=fetcher, wait=lambda: None, clock=lambda: 0.0)
        row = scan.con.execute(
            "SELECT state,reason,bytes_received FROM resource_graph_fetches"
        ).fetchone()

    assert calls == ["https://example.test/redirect.js"]
    assert totals == {"stored": 0, "fetched": 0, "excluded": 0, "failed": 0, "budget": 1}
    assert tuple(row) == ("budget", "resource graph request budget exhausted", 3)


def test_graph_byte_budget_counts_redirect_entity_before_following_it(tmp_path):
    path = tmp_path / "scan.sqlite"
    calls: list[str] = []
    settings = load(
        overrides={
            "storage.format_version": "scan.v2",
            "resources.fetch": True,
            "resources.graph.max_requests": 10,
            "resources.graph.max_bytes": 1,
            "resources.graph.max_bytes_per_resource": 8,
            "resources.graph.max_redirects": 2,
        }
    )

    def fetcher(url: str):
        calls.append(url)
        return _Response(302, b"x", {"location": "/final.js"})

    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<script src="/redirect.js"></script>')
        totals = capture(scan, settings, fetcher=fetcher, wait=lambda: None, clock=lambda: 0.0)

    assert calls == ["https://example.test/redirect.js"]
    assert totals == {"stored": 0, "fetched": 0, "excluded": 0, "failed": 0, "budget": 1}


def test_requeue_invalidation_discards_old_page_graph_and_refetches_shared_resource(tmp_path):
    path = tmp_path / "scan.sqlite"
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        first_id = _store_html(scan, '<img src="/shared.png">')
        first_document = scan.con.execute(
            "SELECT document_id FROM documents WHERE url_id=?", (first_id,)
        ).fetchone()[0]
        scan.enqueue([("https://example.test/other", 1)])
        lease = scan.claim(1)[0]
        html = '<img src="/shared.png">'
        scan.commit_page(
            lease,
            _record(lease.url),
            captures=[_event(lease.url, html.encode())],
            runtime=_runtime(),
        )
        second_document = scan.con.execute(
            "SELECT document_id FROM documents WHERE url_id=?", (lease.url_id,)
        ).fetchone()[0]
        store_document(
            scan.con,
            page_url_id=lease.url_id,
            source_document_id=second_document,
            representation="static",
            html=html,
        )
        capture(
            scan,
            settings,
            fetcher=lambda _url: _Response(200, b"image", {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        invalidate_pages(scan.con, [first_id])
        remaining = scan.con.execute(
            "SELECT state,reason FROM resource_graph_occurrences WHERE page_url_id=?",
            (lease.url_id,),
        ).fetchone()
        fetch_count = scan.con.execute("SELECT COUNT(*) FROM resource_graph_fetches").fetchone()[0]
        old_context = scan.con.execute(
            "SELECT 1 FROM context_items WHERE kind='resource_graph_coverage' AND item_key=?",
            (f"document:{first_document}",),
        ).fetchone()

    assert tuple(remaining) == ("disabled", "resource fetch invalidated by requeue")
    assert fetch_count == 0
    assert old_context is None


def test_v1_and_read_only_v2_access_do_not_create_or_upgrade_resource_graph(tmp_path):
    v1_path = tmp_path / "v1.sqlite"
    with NativeScan.create(v1_path, **_metadata()) as scan:
        assert scan.con.execute("PRAGMA user_version").fetchone()[0] == 1
        assert read(scan.con) == {
            "state": "unavailable",
            "reason": "resource graph was not stored in this scan",
        }
        assert (
            scan.con.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'resource_graph_%'"
            ).fetchone()[0]
            == 0
        )

    con = sqlite3.connect(tmp_path / "reader-v2.sqlite")
    try:
        con.execute("PRAGMA user_version=2")
        assert read(con) == {"state": "unavailable", "reason": "resource graph extension is absent"}
        assert (
            con.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'resource_graph_%'"
            ).fetchone()[0]
            == 0
        )
    finally:
        con.close()


def test_resource_read_exposes_per_kind_declaration_and_weight_aggregates(tmp_path):
    path = tmp_path / "aggregates.sqlite"
    settings = load(
        overrides={
            "storage.format_version": "scan.v2",
            "resources.fetch": True,
            "speed.min_delay_seconds": 0,
        }
    )

    def fetcher(url: str):
        return _Response(200, b"resource", {"content-type": "image/png"})

    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/image.png">')
        capture(scan, settings, fetcher=fetcher, wait=lambda: None, clock=lambda: 0.0)
        result = read(scan.con)

    assert result["aggregates"]["occurrences_by_kind"] == {"image": 1}
    assert result["aggregates"]["blocks_by_state"] == {"complete": 1}
    assert result["aggregates"]["unique_resource_weight_by_kind"]["image"] == {
        "unique_urls": 1,
        "complete_urls": 1,
        "unmeasured_urls": 0,
        "bytes_received": len(b"resource"),
        "bytes_state": "exact",
    }


def _v2_crawl(path, fetcher):
    settings = load(
        overrides={
            "storage.format_version": "scan.v2",
            "resources.fetch": True,
            "speed.min_delay_seconds": 0,
            "limits.max_urls": 1,
            "limits.max_depth": 1,
            "resources.graph.max_requests": 10,
        }
    )
    crawl_to_scan(
        "https://example.test/",
        scan_out=str(path),
        settings=settings,
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions={
            "python": "test",
            "sqlite": "test",
            "httpx": "test",
            "lxml": "test",
            "beautifulsoup4": "test",
        },
        fetcher=fetcher,
        sleeper=lambda _seconds: None,
    )
    return settings


def _v2_site(url: str):
    if url.endswith("/robots.txt"):
        return _Response(
            200, b"User-agent: SEOHEAD-Tools\nAllow: /\n", {"content-type": "text/plain"}
        )
    if url.endswith("/site.css"):
        return _Response(200, b".hero{background:url('/asset.png')}", {"content-type": "text/css"})
    if url.endswith("/asset.png"):
        return _Response(200, b"not-an-image", {"content-type": "image/png"})
    return _Response(
        200,
        b'<html><head><link rel="stylesheet" href="/site.css"></head><body>page</body></html>',
        {"content-type": "text/html"},
    )


def test_v2_crawl_reopens_with_closed_resource_graph_coverage_and_css_children(tmp_path):
    path = tmp_path / "v2.sqlite"
    _v2_crawl(path, _v2_site)

    with open_scan(path, require_audit=False) as con:
        graph = read(con)
        coverage = con.execute(
            "SELECT payload_json,completeness,reason FROM context_items "
            "WHERE kind='resource_graph_coverage'"
        ).fetchone()

    assert graph["state"] == "complete"
    assert [(row["carrier"], row["nesting_depth"]) for row in graph["occurrences"]] == [
        ("link[href]", 0),
        ("css", 1),
    ]
    assert json.loads(coverage[0])["state"] == "complete"
    assert tuple(coverage[1:]) == ("complete", "")


def test_v2_requeue_then_resume_replaces_active_graph_and_reanalysis_stays_offline(
    tmp_path, monkeypatch
):
    from seohead.mcp.reanalysis_handlers import reanalyze_scan

    path, backup, derived = (
        tmp_path / "v2.sqlite",
        tmp_path / "before.sqlite",
        tmp_path / "derived.sqlite",
    )
    calls: list[str] = []

    def fetcher(url: str):
        calls.append(url)
        return _v2_site(url)

    settings = _v2_crawl(path, fetcher)
    requeue_scan(path, where='url = "https://example.test/"', backup_path=backup)
    assert backup.exists()
    with open_scan(path, require_audit=False) as con:
        assert read(con)["total"] == 0
    before_resume = len(calls)
    crawl_to_scan(
        "https://example.test/",
        scan_out=str(path),
        settings=settings,
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions={
            "python": "test",
            "sqlite": "test",
            "httpx": "test",
            "lxml": "test",
            "beautifulsoup4": "test",
        },
        fetcher=fetcher,
        sleeper=lambda _seconds: None,
    )
    assert len(calls) > before_resume
    with open_scan(path, require_audit=False) as con:
        assert read(con)["total"] == 2

    monkeypatch.setattr("socket.getaddrinfo", lambda *_args, **_kwargs: pytest.fail("no network"))
    resumed_digest = hashlib.sha256(path.read_bytes()).hexdigest()
    reanalyze_scan(str(path), str(derived), producer_build="b" * 40)
    assert hashlib.sha256(path.read_bytes()).hexdigest() == resumed_digest
    with open_scan(derived, require_audit=False) as con:
        assert con.execute("SELECT source_kind FROM scan").fetchone()[0] == "reanalysis"

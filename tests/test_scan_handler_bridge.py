"""Finite, network-free handler bridge coverage for native scan collection."""

from __future__ import annotations

from contextlib import contextmanager
from types import SimpleNamespace

import pytest

from seohead.crawl.sqlite_adapter import ScanRun
from seohead.mcp import scan_handlers
from tests.test_scan_artifact_office import frozen_office_clock as frozen_office_clock


class _Con:
    def __init__(self, counts, has_audit=False):
        self.counts = counts
        self.has_audit = has_audit

    def execute(self, statement, _params=()):
        if "FROM audit" in statement:
            return SimpleNamespace(fetchone=lambda: (1,) if self.has_audit else None)
        table = statement.rsplit(" ", 1)[-1]
        return SimpleNamespace(fetchone=lambda: (self.counts[table],))


class _Scan:
    current = None

    def __init__(self, counts):
        self.con = _Con(counts)
        self.saved = None
        self.finished = []
        self.audit_unavailable = None

    @classmethod
    def open(cls, *_args, **_kwargs):
        return cls.current

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return None

    def save_audit(self, audit):
        self.saved = audit

    def note_audit_unavailable(self, reason):
        self.audit_unavailable = reason

    def finish_capture(self, *, reason):
        self.finished.append(reason)
        return True

    def resume_snapshot(self, *, include_edges=False):
        return {"counts": {**self.con.counts, "queued": 0}}

    def sitemap_roots(self):
        return []


def _run(**overrides):
    fields = {
        "path": "scan.sqlite",
        "pages": 1,
        "links": 1,
        "forms": 0,
        "lifecycle": "running",
        "finish_reason": "finished",
        "partial": False,
        "start_page_gate": {"html": "<html></html>", "outlinks": 0, "external_outlinks": 0},
    }
    fields.update(overrides)
    return ScanRun(**fields)


@pytest.fixture
def bridge(monkeypatch):
    scan = _Scan({"pages": 1, "links": 1, "forms": 0})
    _Scan.current = scan
    monkeypatch.setattr("seohead.storage.native_scan.NativeScan", _Scan)
    monkeypatch.setattr(
        scan_handlers, "_producer_provenance", lambda _build: ("test", "a" * 40, {})
    )
    monkeypatch.setattr(
        "seohead.crawl.sqlite_adapter.crawl_to_scan", lambda *_args, **_kwargs: _run()
    )

    @contextmanager
    def empty_reconciliation(*_args, **_kwargs):
        yield SimpleNamespace(available=False, reason="no saved sitemap declarations")

    monkeypatch.setattr(
        "seohead.crawl.sql_sitemap.prepare_sitemap_reconciliation", empty_reconciliation
    )
    return scan


@pytest.mark.parametrize("stored_links", [1, 1_500_000])
def test_stored_graph_size_does_not_block_page_audit(bridge, monkeypatch, stored_links):
    from seohead.crawl.spider import SpiderResult

    result = SpiderResult()
    bridge.con.counts["links"] = stored_links
    captured = {}

    def rebuild(_scan, **kwargs):
        captured["page_view"] = kwargs.get("page_view")
        return result

    monkeypatch.setattr(scan_handlers, "_rebuild_page_result", rebuild)

    def audit_bridge(actual, **kwargs):
        captured["result"] = actual
        captured["kwargs"] = kwargs
        return {"ignored": True}, {"schema_version": "2.0", "pages": []}

    monkeypatch.setattr("seohead.mcp.handlers._audit_crawl_result", audit_bridge)
    response = scan_handlers.crawl_site_scan(
        "https://example.test/",
        scan_out="scan.sqlite",
        settings={"robots": {"policy": "respect"}},
        producer_build="a" * 40,
    )
    assert response["audit_available"] is True
    assert bridge.saved == {"schema_version": "2.0", "pages": []}
    assert bridge.finished == ["finished"]
    assert captured["result"].start_page_evidence == {
        "html": "<html></html>",
        "outlinks": 0,
        "external_outlinks": 0,
    }
    assert captured["page_view"] is True
    assert captured["kwargs"]["out_dir"] is None


@pytest.mark.parametrize("pages,forms", [(10_001, 0), (1, 20_001), (1_000_000, 2_000_000)])
def test_streaming_bridge_accepts_page_and_form_populations(pages, forms):
    counts = {"pages": pages, "forms": forms}
    assert (
        scan_handlers._bridge_reason(
            counts, {"html": "<html></html>", "outlinks": 0, "external_outlinks": 0}
        )
        is None
    )
    assert "not retained" in scan_handlers._bridge_reason(counts, None)


def test_large_js_render_uses_a_cursor_backed_page_view(bridge, monkeypatch):
    pages = 10_001
    bridge.con.counts["pages"] = pages
    monkeypatch.setattr(
        "seohead.crawl.sqlite_adapter.crawl_to_scan",
        lambda *_args, **_kwargs: _run(pages=pages),
    )
    monkeypatch.setattr(
        scan_handlers,
        "_rebuild_page_result",
        lambda _scan, **kwargs: SimpleNamespace(
            pages=scan_handlers._StoredPages(_scan.con),
            partial=False,
            stopped_reason="",
            robots_blocked=[],
            seed_urls=[],
            limitations=[],
        ),
    )
    monkeypatch.setattr(
        bridge,
        "resume_snapshot",
        lambda **_kwargs: {
            "counts": {**bridge.con.counts, "queued": 0},
            "scan": {
                "crawl_partial": 0,
                "limitations_json": "[]",
                "corpus_partial": 0,
                "capabilities_json": "{}",
            },
        },
    )
    monkeypatch.setattr("seohead.storage.corpus.rendered_body_retention", lambda _con: {})
    observed = []

    def render(_scan, result, _settings, **_kwargs):
        observed.append(result.pages)
        assert isinstance(result.pages, scan_handlers._StoredPages)

    monkeypatch.setattr("seohead.crawl.sqlite_render.run_render_escalation", render)
    monkeypatch.setattr(
        "seohead.mcp.handlers._audit_crawl_result",
        lambda *_args, **_kwargs: ({}, {"schema_version": "2.0", "pages": []}),
    )

    response = scan_handlers.crawl_site_scan(
        "https://example.test/",
        scan_out="scan.sqlite",
        settings={
            "robots": {"policy": "respect"},
            "rendering": {"mode": "render", "rendered_links": {"crawl": True}},
        },
        producer_build="a" * 40,
    )

    assert response["audit_available"] is True
    assert len(observed) == 1
    assert bridge.saved is not None


def test_stored_page_view_iterates_and_resolves_exact_urls(tmp_path):
    from seohead.storage import open_scan
    from tests.test_scan_corpus_inputs import _scan

    url_a = "https://example.test/a"
    url_b = "https://example.test/b"
    path = _scan(
        tmp_path,
        [
            (url_a, "<html><body><main>A</main></body></html>"),
            (url_b, "<html><body><main>B</main></body></html>"),
        ],
    )
    with open_scan(path, require_audit=False) as con:
        pages = scan_handlers._StoredPages(con)
        assert len(pages) == 2
        assert [page.url for page in pages] == [url_a, url_b]
        assert pages.get(url_b).url == url_b
        assert pages.get("https://example.test/missing") is None


def test_resumed_scan_without_transient_html_is_named_no_audit(bridge, monkeypatch):
    monkeypatch.setattr(
        "seohead.crawl.sqlite_adapter.crawl_to_scan",
        lambda *_args, **_kwargs: _run(start_page_gate=None),
    )
    monkeypatch.setattr(
        scan_handlers,
        "_rebuild_page_result",
        lambda _scan, **kwargs: SimpleNamespace(
            pages=scan_handlers._StoredPages(_scan.con),
            partial=False,
            stopped_reason="",
            robots_blocked=[],
            seed_urls=[],
            limitations=[],
        ),
    )
    response = scan_handlers.crawl_site_scan(
        "https://example.test/",
        scan_out="scan.sqlite",
        settings={"robots": {"policy": "respect"}},
        producer_build="a" * 40,
    )
    assert response["audit_available"] is False
    assert "start-page" in response["audit_reason"]
    assert bridge.finished


def test_interrupted_finalization_reuses_an_already_saved_audit(bridge, monkeypatch):
    bridge.con.has_audit = True
    monkeypatch.setattr(
        "seohead.crawl.sqlite_adapter.crawl_to_scan",
        lambda *_args, **_kwargs: _run(start_page_gate=None),
    )
    monkeypatch.setattr(
        scan_handlers,
        "_rebuild_page_result",
        lambda _scan, **kwargs: SimpleNamespace(
            pages=scan_handlers._StoredPages(_scan.con),
            partial=False,
            stopped_reason="",
            robots_blocked=[],
            seed_urls=[],
            limitations=[],
        ),
    )
    response = scan_handlers.crawl_site_scan(
        "https://example.test/",
        scan_out="scan.sqlite",
        settings={"robots": {"policy": "respect"}},
        producer_build="a" * 40,
    )
    assert response["audit_available"] is True
    assert "reused" in response["audit_reason"]
    assert bridge.saved is None and bridge.finished == ["finished"]


def test_explicit_producer_build_never_invokes_git(monkeypatch):
    monkeypatch.setattr("subprocess.run", lambda *_args, **_kwargs: pytest.fail("git invoked"))
    _version, revision, runtime = scan_handlers._producer_provenance("a" * 40)
    assert revision == "a" * 40
    assert set(runtime) == {"python", "sqlite", "httpx", "lxml", "beautifulsoup4"}


def test_invalid_native_mode_does_not_start_sitemap_requests(tmp_path, monkeypatch):
    from seohead.crawl.settings import load

    monkeypatch.setattr(
        "seohead.mcp.scan_sitemaps.load_sitemaps",
        lambda *args, **kwargs: pytest.fail("invalid mode fetched a sitemap"),
    )
    with pytest.raises(ValueError, match=r"cache\.mode=off"):
        scan_handlers.crawl_site_scan(
            "https://example.test/",
            scan_out=str(tmp_path / "scan.sqlite"),
            settings=load(overrides={"cache.mode": "live"}),
            sitemap="https://example.test/sitemap.xml",
            producer_build="a" * 40,
        )
    assert not (tmp_path / "scan.sqlite").exists()


@pytest.mark.parametrize("min_delay", [0.0, 0.01])
def test_real_handler_adapter_native_audit_and_all_report_formats(
    tmp_path, monkeypatch, frozen_office_clock, min_delay
):
    """The handler uses injected transport, but every storage and audit layer is real."""
    import httpx

    from seohead.checks import sitemap as sitemap_tool
    from seohead.crawl import sqlite_adapter
    from seohead.crawl.collect import fetch_one as real_fetch_one
    from seohead.crawl.settings import load
    from seohead.crawl.spider import _fetch_robots as real_fetch_robots
    from seohead.reports import build_report
    from seohead.sf.core import sitemap_coverage
    from seohead.storage import open_scan, read_audit

    sitemap_url = "https://example.test/sitemap.xml"
    documents = {
        "https://example.test/robots.txt": (
            "text/plain",
            "User-agent: SEOHEAD-Tools\nAllow: /\nSitemap: https://example.test/sitemap.xml\n",
        ),
        "https://example.test/": (
            "text/html",
            "<html><head><title>Home</title></head><body><main><h1>Home</h1>"
            "<a href='/next'>Next</a><form method='post' action='/send'>"
            "<input type='password'></form></main></body></html>",
        ),
        "https://example.test/next": (
            "text/html",
            "<html><head><title>Next</title></head><body><h1>Next</h1></body></html>",
        ),
    }

    class Response:
        def __init__(self, url):
            content_type, text = documents[url]
            self.status_code = 200
            self.text = text
            self.content = text.encode("utf-8")
            self.headers = {"content-type": content_type}

    def collector_transport(url):
        return Response(url)

    def fetch_with_injected_transport(url, **kwargs):
        kwargs.pop("fetcher", None)
        return real_fetch_one(url, fetcher=collector_transport, **kwargs)

    @contextmanager
    def no_client(*_args, **_kwargs):
        yield None

    def robots_with_injected_transport(start, _fetcher, _client, *, wait=None):
        return real_fetch_robots(start, collector_transport, None, wait=wait)

    def sitemap_response(request):
        if str(request.url).endswith("robots.txt"):
            return httpx.Response(200, text=documents["https://example.test/robots.txt"][1])
        if str(request.url) == sitemap_url:
            return httpx.Response(
                200,
                text=(
                    "<?xml version='1.0'?><urlset "
                    "xmlns='http://www.sitemaps.org/schemas/sitemap/0.9'>"
                    "<url><loc>https://example.test/</loc></url>"
                    "<url><loc>https://example.test/next</loc></url></urlset>"
                ),
            )
        return httpx.Response(404)

    monkeypatch.setattr(sqlite_adapter, "_client_context", no_client)
    monkeypatch.setattr(sqlite_adapter, "fetch_one", fetch_with_injected_transport)
    monkeypatch.setattr(sqlite_adapter, "_fetch_robots", robots_with_injected_transport)
    monkeypatch.setattr(sitemap_coverage, "validate_url", lambda _url: None)

    def mock_client(*_args, **_kwargs):
        return httpx.Client(transport=httpx.MockTransport(sitemap_response)), False

    monkeypatch.setattr(
        sitemap_coverage,
        "http_client",
        mock_client,
    )
    monkeypatch.setattr(sitemap_tool, "http_client", mock_client)

    scan = tmp_path / "scan.sqlite"
    response = scan_handlers.crawl_site_scan(
        "https://example.test/",
        scan_out=str(scan),
        settings=load(overrides={"speed.min_delay_seconds": min_delay}),
        sitemap=sitemap_url,
        producer_build="a" * 40,
    )
    assert response["audit_available"] is True
    assert response["urls_collected"] == 2
    assert response["links_collected"] == 1
    assert response["forms_collected"] == 1

    with open_scan(scan) as con:
        assert (
            con.execute(
                "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declared_url'"
            ).fetchone()[0]
            == 2
        )
        assert (
            con.execute(
                "SELECT completeness FROM context_items WHERE kind='sitemap_fetch_summary'"
            ).fetchone()[0]
            == "complete"
        )

    audit = read_audit(scan)
    desync = [item for item in audit["issues"] if item["check"] == "SITEMAP_DESYNC"]
    # Both declared pages were fetched. The start page does not need an incoming
    # link to avoid a false orphan/desync finding in a retained native audit.
    assert not desync
    assert audit["summary"]["sitemap"]["urls_in_sitemap"] == 2
    assert audit["summary"]["sitemap"]["sitemap_status_unmeasured"] == 0
    assert audit["summary"]["sitemap"]["sitemap_orphan_findings_count"] == 0
    assert audit["run"]["effective_max_requests_per_second"] == (
        "unbounded" if min_delay == 0 else 100.0
    )
    assert audit["pages"] and audit["summary"]["totals"]["urls_crawled"] == 2
    for fmt in ("json", "md", "csv", "xlsx", "docx"):
        from_scan = tmp_path / "from-scan" / f"report.{fmt}"
        from_document = tmp_path / "from-document" / f"report.{fmt}"
        assert build_report(str(scan), fmt, str(from_scan))["ok"]
        assert build_report(audit, fmt, str(from_document))["ok"]
        if fmt == "json":
            import json

            assert json.loads(from_scan.read_text()) == json.loads(from_document.read_text())
        else:
            assert from_scan.read_bytes() == from_document.read_bytes()
        if fmt == "csv":
            for suffix in (".pages.csv", ".scope.csv"):
                scan_sidecar = from_scan.with_suffix(suffix)
                document_sidecar = from_document.with_suffix(suffix)
                assert scan_sidecar.exists() == document_sidecar.exists()
                if scan_sidecar.exists():
                    assert scan_sidecar.read_bytes() == document_sidecar.read_bytes()

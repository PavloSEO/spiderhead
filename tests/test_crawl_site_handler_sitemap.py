"""Handler wiring for sitemap-seeded crawl mode. No network.

The spider's own BFS and ``sitemap.crawl()``'s XML parsing are already covered
elsewhere; this only proves ``handlers.crawl_site`` wires the two together and
that the reconciliation lands in ``audit.json`` under the same key the
Screaming Frog pipeline uses (``summary.sitemap``), with the SITEMAP_ORPHAN and
URL_NOT_IN_SITEMAP check ids that pipeline already defines.
"""

from __future__ import annotations

import pytest

import seohead.checks.sitemap as sitemap_tool
from seohead.crawl.collect import CrawlResult, PageRecord
from seohead.crawl.spider import LinkEdge, SpiderResult
from seohead.mcp import handlers

DECLARED = [f"https://example.com/p{i}" for i in range(1, 9)] + [
    "https://example.com/p9",
    "https://example.com/p10",
]
LINKED = DECLARED[:8]
ORPHANED = DECLARED[8:]


def _fake_spider_result() -> SpiderResult:
    result = SpiderResult()
    result.pages = [PageRecord(url=u, status_code=200, content_type="text/html") for u in DECLARED]
    result.pages.append(
        PageRecord(url="https://example.com/extra", status_code=200, content_type="text/html")
    )
    # Every linked URL, plus one the sitemap never declares, is reachable by
    # following a link from the home page. The two orphans never appear here.
    result.links = [
        LinkEdge(source="https://example.com/", destination=u, anchor="", nofollow=False)
        for u in [*LINKED, "https://example.com/extra"]
    ]
    result.seed_urls = list(DECLARED)
    return result


def test_handler_reconciles_a_sitemap_seeded_crawl(monkeypatch, tmp_path):
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": u} for u in DECLARED]},
    )
    monkeypatch.setattr("seohead.crawl.spider.crawl_site", lambda *a, **kw: _fake_spider_result())

    out = handlers.crawl_site(
        url="https://example.com/", sitemap="https://example.com/sitemap.xml", out_dir=str(tmp_path)
    )

    sitemap_summary = out["summary"]["sitemap"]
    assert sitemap_summary["sitemap_url"] == "https://example.com/sitemap.xml"
    assert sorted(sitemap_summary["in_sitemap_and_linked"]) == sorted(LINKED)
    assert sorted(sitemap_summary["in_sitemap_not_linked"]) == sorted(ORPHANED)
    assert sitemap_summary["linked_not_in_sitemap"] == ["https://example.com/extra"]

    assert out["discovery"]["sitemap_url"] == "https://example.com/sitemap.xml"
    assert out["discovery"]["sitemap_seeded"] == len(DECLARED)

    # The same check ids the Screaming Frog pipeline uses for this distinction,
    # so downstream tooling reading audit.json needs only one schema.
    by_check = out["summary"]["by_check"]
    assert by_check["SITEMAP_ORPHAN"] == len(ORPHANED)
    assert by_check["URL_NOT_IN_SITEMAP"] == 1


def test_handler_passes_one_gate_from_sitemap_seeding_to_page_collection(monkeypatch, tmp_path):
    gates = []

    def seeded_sitemap(_url, *, request_gate=None, **_kwargs):
        gates.append(request_gate)
        return {"urls": [{"loc": "https://example.com/page"}]}

    def spider(*_args, **kwargs):
        gates.append(kwargs["dispatch_gate"].wait_turn)
        return _fake_spider_result()

    monkeypatch.setattr(sitemap_tool, "crawl", seeded_sitemap)
    monkeypatch.setattr("seohead.crawl.spider.crawl_site", spider)

    handlers.crawl_site(
        url="https://example.com/", sitemap="https://example.com/sitemap.xml", out_dir=str(tmp_path)
    )

    assert gates[0].__self__ is gates[1].__self__


@pytest.mark.parametrize("target", ["https://native.example.test/", "native.example.test"])
def test_handler_warns_when_native_crawl_ignores_robots(monkeypatch, capsys, target, tmp_path):
    monkeypatch.setattr("seohead.crawl.spider.crawl_site", lambda *_args, **_kwargs: SpiderResult())

    handlers.crawl_site(url=target, robots="ignore", out_dir=str(tmp_path))

    assert "robots.txt bypass enabled for native.example.test" in capsys.readouterr().err


def test_handler_warns_for_each_list_host_when_robots_are_ignored(monkeypatch, capsys, tmp_path):
    monkeypatch.setattr(
        "seohead.crawl.collect.collect_urls", lambda *_args, **_kwargs: CrawlResult()
    )

    handlers.crawl_site(
        urls=["https://first.example.test/", "https://second.example.test/path"],
        robots="ignore",
        out_dir=str(tmp_path),
    )

    warning = capsys.readouterr().err
    assert "robots.txt bypass enabled for first.example.test" in warning
    assert "robots.txt bypass enabled for second.example.test" in warning


def test_handler_warns_for_a_resumed_scan_when_robots_are_ignored(monkeypatch, capsys):
    monkeypatch.setattr(
        "seohead.mcp.scan_handlers.crawl_site_scan", lambda *_args, **_kwargs: {"resumed": True}
    )

    handlers.crawl_site(url="https://resume.example.test/", robots="ignore", scan_out="scan.sqlite")

    assert "robots.txt bypass enabled for resume.example.test" in capsys.readouterr().err


def test_render_escalation_threads_the_shared_gate_to_browser_entry_points(monkeypatch):
    seen = []

    def gate():
        return None

    def probe(_url, *, request_gate=None, **_kwargs):
        seen.append(request_gate)
        return {"ok": True, "js_dependent": True}

    def render(_url, _config, *, request_gate=None, **_kwargs):
        seen.append(request_gate)
        return {"ok": True, "html": "<html><body>rendered</body></html>"}

    monkeypatch.setattr("seohead.checks.render.render_check", probe)
    monkeypatch.setattr("seohead.checks.render.render_document", render)
    result = SpiderResult(pages=[PageRecord(url="https://example.com/", content_type="text/html")])
    config = {
        "mode": "js",
        "browser": {"wait_until": "load", "viewport": "desktop"},
        "escalation": {"sample_per_pattern": 1, "max_render_urls": 1},
    }

    handlers._run_render_escalation(
        result,
        config,
        {"http": {"timeout_seconds": 1, "user_agent": ""}, "output": {"dir": ""}},
        request_gate=gate,
    )

    assert seen == [gate, gate]


def test_handler_without_sitemap_reports_no_sitemap_summary(monkeypatch, tmp_path):
    result = _fake_spider_result()
    result.seed_urls = []
    monkeypatch.setattr("seohead.crawl.spider.crawl_site", lambda *a, **kw: result)

    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))

    assert "sitemap" not in out["summary"]
    assert out["discovery"]["sitemap_url"] is None
    assert out["discovery"]["sitemap_seeded"] == 0


# ── URL_NOT_IN_SITEMAP compares pages with pages (#94) ───────────────────────


def _site_with_the_four_wrong_populations() -> SpiderResult:
    """One real omission, surrounded by the four kinds of URL that are not one.

    Each of these was reported as a sitemap defect on a live site: an outbound link, a
    gallery link straight to an image file, a URL the crawl recorded a link to but never
    fetched (budget or scope), and a page the site deliberately marked noindex.
    """
    result = SpiderResult()
    result.pages = [
        PageRecord(url="https://example.com/", status_code=200, content_type="text/html"),
        PageRecord(url="https://example.com/declared", status_code=200, content_type="text/html"),
        # The one real finding: an indexable page, linked, that the sitemap forgot.
        PageRecord(url="https://example.com/undeclared", status_code=200, content_type="text/html"),
        PageRecord(
            url="https://example.com/gallery/photo.jpg", status_code=200, content_type="image/jpeg"
        ),
        PageRecord(
            url="https://example.com/private",
            status_code=200,
            content_type="text/html",
            meta_robots="noindex, follow",
        ),
        PageRecord(url="https://example.com/gone", status_code=404, content_type="text/html"),
    ]
    result.links = [
        LinkEdge(source="https://example.com/", destination=destination, anchor="", nofollow=False)
        for destination in (
            "https://example.com/declared",
            "https://example.com/undeclared",
            "https://example.com/gallery/photo.jpg",
            "https://example.com/private",
            "https://example.com/gone",
            "https://wa.me/1234567890",  # outbound
            "https://example.com/never-fetched",  # linked, outside the budget
        )
    ]
    result.seed_urls = ["https://example.com/declared"]
    return result


def test_only_a_missing_page_is_reported_not_files_hosts_or_uncrawled_urls(monkeypatch, tmp_path):
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {
            "urls": [{"loc": "https://example.com/"}, {"loc": "https://example.com/declared"}]
        },
    )
    monkeypatch.setattr(
        "seohead.crawl.spider.crawl_site", lambda *a, **kw: _site_with_the_four_wrong_populations()
    )

    out = handlers.crawl_site(
        url="https://example.com/", sitemap="https://example.com/sitemap.xml", out_dir=str(tmp_path)
    )
    summary = out["summary"]["sitemap"]

    assert summary["linked_not_in_sitemap"] == ["https://example.com/undeclared"]
    assert out["summary"]["by_check"].get("URL_NOT_IN_SITEMAP") == 1

    # Nothing is silently discarded: what was set aside is named.
    set_aside = set(summary["linked_not_comparable"])
    assert "https://wa.me/1234567890" in set_aside
    assert "https://example.com/gallery/photo.jpg" in set_aside
    assert "https://example.com/never-fetched" in set_aside
    assert "https://example.com/private" in set_aside
    assert "https://example.com/gone" in set_aside


def test_narrowing_the_comparable_side_does_not_invent_orphans(monkeypatch, tmp_path):
    """A declared URL that is noindex, or not HTML, is still reachable — so it must not
    turn into a SITEMAP_ORPHAN just because it left the URL_NOT_IN_SITEMAP population."""
    declared = ["https://example.com/private", "https://example.com/gallery/photo.jpg"]
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": u} for u in declared]},
    )
    monkeypatch.setattr(
        "seohead.crawl.spider.crawl_site", lambda *a, **kw: _site_with_the_four_wrong_populations()
    )

    out = handlers.crawl_site(
        url="https://example.com/", sitemap="https://example.com/sitemap.xml", out_dir=str(tmp_path)
    )

    assert out["summary"]["sitemap"]["in_sitemap_not_linked"] == []
    assert "SITEMAP_ORPHAN" not in out["summary"]["by_check"]


# ── auto-discovery of more than one declared sitemap (#200) ─────────────────


def test_auto_discovery_seeds_from_every_declared_sitemap(monkeypatch, tmp_path):
    """robots.txt can declare independent sitemaps for different content (pages,
    products, ...); auto-discovery must expand every one of them, not just the
    first, and seed the crawl from their de-duplicated URL union."""
    import json

    import seohead.checks.robots as robots_tool

    first = "https://example.com/sitemap-pages.xml"
    second = "https://example.com/sitemap-products.xml"
    calls: list[str] = []

    monkeypatch.setattr(
        robots_tool,
        "check_robots",
        lambda url, **_kwargs: {"ok": True, "sitemaps": [first, second]},
    )

    def fake_sitemap_crawl(url: str, concurrency: int = 3, **_kwargs) -> dict:
        calls.append(url)
        loc = "https://example.com/page-a" if url == first else "https://example.com/product-b"
        return {"urls": [{"loc": loc}]}

    monkeypatch.setattr(sitemap_tool, "crawl", fake_sitemap_crawl)

    seeds: list[str] = []

    def fake_spider(*_a: object, **kw: object) -> SpiderResult:
        seeds.extend(kw.get("seed_urls") or [])
        result = SpiderResult()
        result.pages = [
            PageRecord(url="https://example.com/", status_code=200, content_type="text/html")
        ]
        result.seed_urls = list(kw.get("seed_urls") or [])
        return result

    monkeypatch.setattr("seohead.crawl.spider.crawl_site", fake_spider)

    config_path = tmp_path / "crawl.json"
    config_path.write_text(json.dumps({"sitemaps": {"auto_discover": True}}))

    out = handlers.crawl_site(
        url="https://example.com/", config=str(config_path), out_dir=str(tmp_path / "legacy")
    )

    assert calls == [first, second]
    assert seeds == ["https://example.com/page-a", "https://example.com/product-b"]
    assert out["discovery"]["sitemap_urls"] == [first, second]


# ── the direct protocol audit reaches every discovered root (#311) ──────────


def test_direct_audit_fetches_every_auto_discovered_root_not_just_the_first(monkeypatch, tmp_path):
    """#311: auto-discovery keeps every ``Sitemap:`` directive for crawl seeding, and the
    live protocol audit (SITEMAP_URL_DUPLICATED, SITEMAP_FETCH_INCOMPLETE, ...) must reach
    every one of them too, not only the first root handed to it."""
    import seohead.checks.robots as robots_tool
    import seohead.sf.core.sitemap_coverage as sitemap_coverage

    base = "https://example.com"
    first = f"{base}/pages.xml"
    second = f"{base}/products.xml"
    shared = f"{base}/shared"

    monkeypatch.setattr(
        robots_tool,
        "check_robots",
        lambda url, **_kwargs: {"ok": True, "sitemaps": [first, second]},
    )

    def fake_sitemap_crawl(url: str, concurrency: int = 3, **_kwargs) -> dict:
        return {"urls": [{"loc": shared}]}

    monkeypatch.setattr(sitemap_tool, "crawl", fake_sitemap_crawl)

    def fake_spider(*_a: object, **kw: object) -> SpiderResult:
        result = SpiderResult()
        result.pages = [
            PageRecord(url="https://example.com/", status_code=200, content_type="text/html")
        ]
        result.seed_urls = list(kw.get("seed_urls") or [])
        return result

    monkeypatch.setattr("seohead.crawl.spider.crawl_site", fake_spider)

    fetch_calls: list[str] = []

    def urlset(loc: str) -> bytes:
        return (
            '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">'
            f"<url><loc>{loc}</loc></url></urlset>"
        ).encode()

    def fake_fetch(url, ua, timeout, retries=2, **_kwargs):
        fetch_calls.append(url)
        return {
            f"{base}/robots.txt": f"Sitemap: {first}\nSitemap: {second}\n".encode(),
            first: urlset(shared),
            second: urlset(shared),
        }.get(url)

    monkeypatch.setattr(sitemap_coverage, "_fetch", fake_fetch)

    import json

    config_path = tmp_path / "auto-discover.json"
    config_path.write_text(json.dumps({"sitemaps": {"auto_discover": True}}))
    out_dir = tmp_path / "out"
    out_dir.mkdir()

    out = handlers.crawl_site(url=f"{base}/", config=str(config_path), out_dir=str(out_dir))

    assert out["discovery"]["sitemap_urls"] == [first, second]
    # Both roots were actually fetched by the direct audit, not just the first.
    assert first in fetch_calls
    assert second in fetch_calls
    # Both declare the same URL, so it is duplicated across two sitemap documents.
    assert out["summary"]["by_check"].get("SITEMAP_URL_DUPLICATED") == 1
    audit = json.loads((out_dir / "audit.json").read_text(encoding="utf-8"))
    dup_issues = [i for i in audit["issues"] if i["check"] == "SITEMAP_URL_DUPLICATED"]
    assert len(dup_issues) == 1
    assert sorted(dup_issues[0]["details"]["sitemaps"]) == sorted([first, second])


def test_direct_audit_with_a_single_explicit_sitemap_is_unaffected(monkeypatch, tmp_path):
    """Negative control for #311: an explicit ``--sitemap`` (single source, no
    auto-discovery) keeps auditing only that one document -- no duplicate finding
    materializes out of thin air."""
    import seohead.sf.core.sitemap_coverage as sitemap_coverage

    base = "https://example.com"
    only = f"{base}/sitemap.xml"

    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": f"{base}/page"}]},
    )

    def fake_spider(*_a: object, **kw: object) -> SpiderResult:
        result = SpiderResult()
        result.pages = [
            PageRecord(url="https://example.com/", status_code=200, content_type="text/html")
        ]
        result.seed_urls = list(kw.get("seed_urls") or [])
        return result

    monkeypatch.setattr("seohead.crawl.spider.crawl_site", fake_spider)

    fetch_calls: list[str] = []

    def fake_fetch(url, ua, timeout, retries=2, **_kwargs):
        fetch_calls.append(url)
        if url == f"{base}/robots.txt":
            return b"User-agent: *\n"
        if url == only:
            return (
                '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">'
                f"<url><loc>{base}/page</loc></url></urlset>"
            ).encode()
        return None

    monkeypatch.setattr(sitemap_coverage, "_fetch", fake_fetch)

    out = handlers.crawl_site(url=f"{base}/", sitemap=only, out_dir=str(tmp_path))

    assert fetch_calls.count(only) == 1
    assert "SITEMAP_URL_DUPLICATED" not in out["summary"]["by_check"]


# ── a report-only robots-blocked page is not both non-indexable and an
#    indexable missing-sitemap page (#316) ──────────────────────────────────


def test_report_only_robots_blocked_page_is_not_reported_as_a_missing_sitemap_page(
    monkeypatch, tmp_path
):
    base = "https://example.com"
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": f"{base}/"}]},
    )

    def fake_spider(*_a: object, **kw: object) -> SpiderResult:
        result = SpiderResult()
        result.pages = [
            PageRecord(url=f"{base}/", status_code=200, content_type="text/html", outlinks=1),
            PageRecord(url=f"{base}/private", status_code=200, content_type="text/html"),
        ]
        result.links = [
            LinkEdge(source=f"{base}/", destination=f"{base}/private", anchor="", nofollow=False)
        ]
        result.robots_blocked = [f"{base}/private"]
        result.seed_urls = []
        return result

    monkeypatch.setattr("seohead.crawl.spider.crawl_site", fake_spider)

    out = handlers.crawl_site(url=f"{base}/", sitemap=f"{base}/sitemap.xml", out_dir=str(tmp_path))
    summary = out["summary"]["sitemap"]

    assert out["summary"]["by_check"].get("URL_NOT_IN_SITEMAP", 0) == 0
    assert f"{base}/private" in summary.get("linked_not_comparable", [])


def test_an_indexable_page_missing_from_the_sitemap_still_fires(monkeypatch, tmp_path):
    """Positive control for #316: an ordinary indexable page the sitemap forgot must
    still be reported -- the robots-blocked exclusion must not swallow real findings."""
    base = "https://example.com"
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": f"{base}/"}]},
    )

    def fake_spider(*_a: object, **kw: object) -> SpiderResult:
        result = SpiderResult()
        result.pages = [
            PageRecord(url=f"{base}/", status_code=200, content_type="text/html", outlinks=1),
            PageRecord(url=f"{base}/undeclared", status_code=200, content_type="text/html"),
        ]
        result.links = [
            LinkEdge(source=f"{base}/", destination=f"{base}/undeclared", anchor="", nofollow=False)
        ]
        result.seed_urls = []
        return result

    monkeypatch.setattr("seohead.crawl.spider.crawl_site", fake_spider)

    out = handlers.crawl_site(url=f"{base}/", sitemap=f"{base}/sitemap.xml", out_dir=str(tmp_path))

    assert out["summary"]["by_check"].get("URL_NOT_IN_SITEMAP") == 1


# ── a partial native crawl withholds the whole-graph SITEMAP_DESYNC verdict
#    (#362) ───────────────────────────────────────────────────────────────


def _partial_graph(*, partial: bool) -> SpiderResult:
    base = "https://example.com"
    result = SpiderResult()
    result.pages = [
        PageRecord(url=f"{base}/", status_code=200, content_type="text/html", outlinks=1),
        PageRecord(url=f"{base}/a", status_code=200, content_type="text/html"),
    ]
    result.links = [LinkEdge(source=f"{base}/", destination=f"{base}/a", anchor="", nofollow=False)]
    result.partial = partial
    result.stopped_reason = "url limit reached before the frontier was exhausted" if partial else ""
    result.finish_reason = "url_limit" if partial else "finished"
    result.seed_urls = []
    return result


def test_partial_crawl_withholds_sitemap_desync_as_a_named_skip(monkeypatch, tmp_path):
    base = "https://example.com"
    declared = [f"{base}/", f"{base}/a", f"{base}/unseen", f"{base}/unseen-2", f"{base}/unseen-3"]
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": u} for u in declared]},
    )
    monkeypatch.setattr(
        "seohead.crawl.spider.crawl_site", lambda *a, **kw: _partial_graph(partial=True)
    )

    out_dir = str(tmp_path)
    out = handlers.crawl_site(url=f"{base}/", sitemap=f"{base}/sitemap.xml", out_dir=out_dir)

    assert out["partial"] is True
    assert "SITEMAP_DESYNC" not in out["summary"]["by_check"]

    import json
    import os

    with open(os.path.join(out_dir, "audit.json"), encoding="utf-8") as handle:
        audit = json.load(handle)
    skipped_ids = {s["id"] for s in audit["run"]["checks_skipped"]}
    assert "SITEMAP_DESYNC" in skipped_ids


def test_complete_crawl_still_fires_sitemap_desync(monkeypatch, tmp_path):
    """Negative control for #362: the identical fixture, minus the partial flag, still
    emits the finding -- withholding is tied to the run being partial, not to sitemap
    seeding in general."""
    base = "https://example.com"
    declared = [f"{base}/", f"{base}/a", f"{base}/unseen", f"{base}/unseen-2", f"{base}/unseen-3"]
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": u} for u in declared]},
    )
    monkeypatch.setattr(
        "seohead.crawl.spider.crawl_site", lambda *a, **kw: _partial_graph(partial=False)
    )

    out = handlers.crawl_site(url=f"{base}/", sitemap=f"{base}/sitemap.xml", out_dir=str(tmp_path))

    assert out["partial"] is False
    assert out["summary"]["by_check"].get("SITEMAP_DESYNC") == 1


def test_unstored_sitemap_seed_preserves_url_problem_findings(monkeypatch, tmp_path):
    import json

    declared = [
        "https://example.com/moved",
        "https://example.com/gone",
        "https://example.com/private",
        "https://example.com/canonicalized",
        "https://example.com/blocked",
        "https://example.com/unseen",
    ]
    result = SpiderResult(
        pages=[
            PageRecord(url=declared[0], status_code=301, content_type="text/html"),
            PageRecord(url=declared[1], status_code=404, content_type="text/html"),
            PageRecord(
                url=declared[2], status_code=200, content_type="text/html", meta_robots="noindex"
            ),
            PageRecord(
                url=declared[3],
                status_code=200,
                content_type="text/html",
                canonical="https://example.com/other",
            ),
            PageRecord(url=declared[4], status_code=200, content_type="text/html"),
        ],
        robots_blocked=[declared[4]],
        seed_urls=declared,
    )
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda *_args, **_kwargs: {"urls": [{"loc": url} for url in declared]},
    )
    monkeypatch.setattr("seohead.crawl.spider.crawl_site", lambda *_args, **_kwargs: result)
    monkeypatch.setattr(
        "seohead.sf.core.sitemap_coverage.run_sitemap", lambda *_args, **_kwargs: {}
    )

    handlers.crawl_site(
        url="https://example.com/", sitemap="https://example.com/sitemap.xml", out_dir=str(tmp_path)
    )
    audit = json.loads((tmp_path / "audit.json").read_text(encoding="utf-8"))
    problems = {
        (item["check"], item["target_url"]): item["details"]
        for item in audit["issues"]
        if item["check"] in {"SITEMAP_URL_3XX", "SITEMAP_URL_4XX_5XX", "SITEMAP_URL_NON_INDEXABLE"}
    }
    assert set(problems) == {
        ("SITEMAP_URL_3XX", declared[0]),
        ("SITEMAP_URL_4XX_5XX", declared[1]),
        ("SITEMAP_URL_NON_INDEXABLE", declared[2]),
        ("SITEMAP_URL_NON_INDEXABLE", declared[3]),
        ("SITEMAP_URL_NON_INDEXABLE", declared[4]),
    }
    assert problems[("SITEMAP_URL_NON_INDEXABLE", declared[3])]["reasons"] == ["non_canonical"]
    assert problems[("SITEMAP_URL_NON_INDEXABLE", declared[4])]["reasons"] == ["robots_blocked"]

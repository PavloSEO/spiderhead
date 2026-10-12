"""The handler layer wires the crawl config into the collector and back into
the audit — this is where a checkpoint path, a duration budget, and a finish
reason actually reach the crawler and the report. No network: the crawler
itself is replaced with a fake."""

import dataclasses
import json

import pytest

import seohead.crawl.spider as spider_mod
from seohead.crawl.collect import PageRecord
from seohead.crawl.spider import LinkEdge, SpiderResult
from seohead.mcp import handlers


def test_out_dir_derives_a_state_path_and_a_config_fingerprint(tmp_path, monkeypatch):
    captured = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return SpiderResult()

    monkeypatch.setattr(spider_mod, "crawl_site", fake)

    handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))

    assert captured["state_path"] == str(tmp_path / "crawl_state.json")
    assert isinstance(captured["config_fingerprint"], str) and captured["config_fingerprint"]
    assert captured["max_seconds"] == 0


def test_no_out_dir_routes_to_the_native_writer(monkeypatch, tmp_path):
    captured = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return SpiderResult()

    monkeypatch.setattr(
        "seohead.mcp.scan_handlers.crawl_site_scan",
        lambda url, **kwargs: captured.update(url=url, **kwargs) or {"scan": kwargs["scan_out"]},
    )
    monkeypatch.chdir(tmp_path)
    result = handlers.crawl_site(url="https://example.com/", producer_build="a" * 40)
    assert result["scan"] == captured["scan_out"]
    assert captured["scan_out"].startswith(str(tmp_path / "scans"))


def test_mcp_profile_name_selects_a_saved_local_profile(tmp_path, monkeypatch):
    from seohead.crawl import profiles

    profile_dir = tmp_path / "profiles"
    monkeypatch.setenv(profiles.DIR_ENV, str(profile_dir))
    source = tmp_path / "config.json"
    source.write_text(json.dumps({"speed": {"concurrency": 2}}), encoding="utf-8")
    profiles.save("catalog", str(source))
    captured = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return SpiderResult()

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    handlers.crawl_site(
        url="https://example.com/", profile="catalog", out_dir=str(tmp_path / "run")
    )
    assert captured["concurrency"] == 2


def test_mcp_profile_and_config_are_mutually_exclusive():
    import pytest

    with pytest.raises(ValueError, match="profile and config are mutually exclusive"):
        handlers.crawl_site(url="https://example.com/", profile="catalog", config="config.json")


def test_urls_file_enters_the_existing_list_collector_in_source_order(tmp_path, monkeypatch):
    """A file is an input source for list mode, never a second discovery mode."""
    source = tmp_path / "urls.txt"
    source.write_text("https://example.com/first\nhttps://example.com/second\n")
    captured = {}

    def fake(urls, **kwargs):
        captured["urls"] = urls
        captured.update(kwargs)
        from seohead.crawl.collect import CrawlResult

        return CrawlResult()

    import seohead.crawl.collect as collect_mod

    monkeypatch.setattr(collect_mod, "collect_urls", fake)
    result = handlers.crawl_site(urls_file=str(source), out_dir=str(tmp_path / "legacy"))

    # The file is streamed to the collector (an iterator), never loaded as a list.
    assert list(captured["urls"]) == ["https://example.com/first", "https://example.com/second"]
    assert result["discovery"]["mode"] == "list"


def test_urls_file_is_not_ambiguous_with_another_crawl_input(tmp_path):
    source = tmp_path / "urls.txt"
    source.write_text("https://example.com/first\n")

    import pytest

    with pytest.raises(ValueError, match="cannot be combined"):
        handlers.crawl_site(url="https://example.com/", urls_file=str(source))


def test_finish_reason_and_resumed_reach_the_handler_output(monkeypatch, tmp_path):
    def fake(*args, **kwargs):
        result = SpiderResult()
        result.finish_reason = "url_limit"
        result.partial = True
        result.resumed = True
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert out["finish_reason"] == "url_limit"
    assert out["resumed"] is True
    assert out["partial"] is True


def test_link_position_classify_defaults_off_and_is_not_computed(monkeypatch, tmp_path):
    captured = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        result = SpiderResult()
        result.links = [LinkEdge("https://example.com/", "https://example.com/a", "", False, "nav")]
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert captured["classify_links"] is False
    assert out["link_position"] == {}


def test_link_position_classify_config_reaches_the_spider_and_the_output(tmp_path, monkeypatch):
    """Issue #20 part 3: link position classification, wired end to end
    through the handler that meets the collector and the audit."""
    config = tmp_path / "crawl.json"
    config.write_text(json.dumps({"link_position": {"classify": True}}))

    def fake(*args, **kwargs):
        captured.update(kwargs)
        result = SpiderResult()
        result.links = [
            LinkEdge("https://example.com/", "https://example.com/orphan", "", False, "nav"),
            LinkEdge("https://example.com/x", "https://example.com/orphan", "", False, "footer"),
            LinkEdge(
                "https://example.com/blog", "https://example.com/linked", "", False, "content"
            ),
        ]
        return result

    captured = {}
    monkeypatch.setattr(spider_mod, "crawl_site", fake)

    out = handlers.crawl_site(
        url="https://example.com/", config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert captured["classify_links"] is True
    boilerplate_only = out["link_position"]["pages_boilerplate_only"]
    assert boilerplate_only == ["https://example.com/orphan"]
    # The same fact also reaches the audit as a registered finding.
    assert out["summary"]["by_check"].get("INLINK_BOILERPLATE_ONLY") == 1


def test_cache_replay_and_stats_reach_the_handler_output_and_the_audit_manifest(
    tmp_path, monkeypatch
):
    """A cached-partly report must say so, both in the immediate result and in audit.json —
    'the site is fine' and 'the site was fine last time we looked' are different claims."""

    def fake(*args, **kwargs):
        result = SpiderResult()
        result.cache_replay = True
        result.cache_stats = {
            "hits": 3,
            "revalidations": 1,
            "stores": 0,
            "bypassed": 0,
            "invalidated": 0,
        }
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))

    assert out["cache_replay"] is True
    assert out["cache_stats"]["hits"] == 3

    audit = json.loads((tmp_path / "audit.json").read_text())
    assert audit["run"]["cache_replay"] is True
    assert audit["run"]["cache_stats"]["revalidations"] == 1


def test_with_no_cache_configured_the_handler_output_says_so_plainly(monkeypatch, tmp_path):
    def fake(*args, **kwargs):
        return SpiderResult()

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert out["cache_replay"] is False
    assert out["cache_stats"] == {}


def test_cache_mode_off_by_default_means_the_spider_receives_no_cache_object(monkeypatch, tmp_path):
    """The default must not create any cache — see the settings-level test for why."""
    captured = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return SpiderResult()

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert captured["cache"] is None


# ── Pre-flight rendering gate (#18) ──────────────────────────────────────────


def _fake_spider_with_start_page(outlinks, external_outlinks, html):
    start = PageRecord(
        url="https://example.com/", outlinks=outlinks, external_outlinks=external_outlinks
    )

    def fake(*args, **kwargs):
        result = SpiderResult()
        result.pages = [start]
        result.start_page_evidence = {
            "html": html,
            "outlinks": outlinks,
            "external_outlinks": external_outlinks,
        }
        return result

    return fake


def test_zero_internal_links_on_the_start_page_requires_rendering(monkeypatch, tmp_path):
    fake = _fake_spider_with_start_page(0, 0, "<html><body>hi</body></html>")
    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert out["requires_rendering"] is True
    assert "zero internal links" in out["requires_rendering_reason"]
    assert out["summary"]["health_score"] is None


def test_an_empty_spa_shell_on_the_start_page_requires_rendering(monkeypatch, tmp_path):
    html = '<html><body><div id="root"></div></body></html>'
    fake = _fake_spider_with_start_page(3, 0, html)
    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert out["requires_rendering"] is True
    assert "empty SPA shell" in out["requires_rendering_reason"]


def test_a_normal_start_page_does_not_require_rendering(monkeypatch, tmp_path):
    fake = _fake_spider_with_start_page(5, 0, "<html><body>hi there</body></html>")
    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert out["requires_rendering"] is False
    assert out["requires_rendering_reason"] == ""


def test_the_gate_applies_even_in_the_default_raw_mode(monkeypatch, tmp_path):
    """Both checks are static-only, so the default (no rendering ever configured)
    still catches the false-green case #18 exists for."""
    fake = _fake_spider_with_start_page(0, 0, "<html></html>")
    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert out["requires_rendering"] is True
    assert out["render_escalation"] == {}


# ── Selective escalation wiring (#18) ────────────────────────────────────────


def _rendering_config_file(tmp_path, mode, **overrides):
    path = tmp_path / "crawl.json"
    config = {"rendering": {"mode": mode, **overrides}}
    path.write_text(json.dumps(config))
    return str(path)


def test_js_mode_escalates_only_the_pattern_that_needs_it(tmp_path, monkeypatch):
    pages = [
        PageRecord(url="https://example.com/", outlinks=1, external_outlinks=0),
        PageRecord(url="https://example.com/app/1", outlinks=0, external_outlinks=0),
        PageRecord(url="https://example.com/app/2", outlinks=0, external_outlinks=0),
    ]
    links = [LinkEdge("https://example.com/", "https://example.com/app/1", "", False)]

    def fake_spider(*args, **kwargs):
        result = SpiderResult()
        result.pages = pages
        result.links = links
        result.start_page_evidence = {"html": "<html><body>hi</body></html>", "outlinks": 1}
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake_spider)

    import seohead.checks.render as render_mod

    def fake_render_check(url, **kwargs):
        return {"ok": True, "js_dependent": "/app/" in url, "empty_shell": None}

    # Accepts whatever the real render_document accepts: the crawl now passes its
    # own User-Agent through (#199), and a stand-in with a narrower signature than
    # the function it replaces fails on the next keyword the real one grows.
    def fake_render_document(url, rendering_config, artifacts_dir=None, **_kw):
        return {
            "ok": True,
            "html": '<html><body><a href="/app/extra">x</a></body></html>',
            "final_url": url,
        }

    monkeypatch.setattr(render_mod, "render_check", fake_render_check)
    monkeypatch.setattr(render_mod, "render_document", fake_render_document)

    config_path = _rendering_config_file(
        tmp_path, "js", escalation={"sample_per_pattern": 1, "max_render_urls": 10}
    )
    out = handlers.crawl_site(
        url="https://example.com/", config=config_path, out_dir=str(tmp_path / "legacy")
    )

    escalation = out["render_escalation"]
    assert escalation["mode"] == "js"
    # One probe for "/" and one for the "/app/*" pattern -- never one per page.
    assert escalation["probe_requests"] == 2
    assert escalation["render_requests"] == 2  # both app/1 and app/2, not just the sample
    assert pages[1].representation == "rendered"
    assert pages[2].representation == "rendered"
    assert pages[0].representation == "static"


def test_legacy_fragment_mode_needs_no_browser(tmp_path, monkeypatch):
    start = PageRecord(url="https://example.com/", outlinks=1, external_outlinks=0)

    def fake_spider(*args, **kwargs):
        result = SpiderResult()
        result.pages = [start]
        result.start_page_evidence = {"html": "<html><body>hi</body></html>", "outlinks": 1}
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake_spider)

    class _FakeResponse:
        def __init__(self, text):
            self.text = text

    class _FakeClient:
        def __init__(self, responses):
            self.responses = responses

        def get(self, url):
            return _FakeResponse(self.responses[url])

        def close(self):
            pass

    responses = {
        "https://example.com/": '<meta name="fragment" content="!">',
        "https://example.com/?_escaped_fragment_=": "<html><body>fully rendered</body></html>",
    }

    import seohead.recon.net as net_mod

    monkeypatch.setattr(
        net_mod, "http_client", lambda timeout, **kw: (_FakeClient(responses), True)
    )
    # No real DNS: the address guard is exercised on its own (test_render.py,
    # test_crawl_safety.py), this test is only about the legacy-fragment wiring.
    monkeypatch.setattr(net_mod, "validate_url", lambda url: url)

    config_path = _rendering_config_file(tmp_path, "legacy_fragment")
    out = handlers.crawl_site(
        url="https://example.com/", config=config_path, out_dir=str(tmp_path / "legacy")
    )

    assert out["render_escalation"]["mode"] == "legacy_fragment"
    assert start.representation == "legacy_fragment"


def test_raw_mode_never_imports_playwright(tmp_path, monkeypatch):
    """The default mode must not touch Playwright at all -- browser rendering
    is never required for the test suite, and this proves the import path is
    not even attempted."""
    import builtins

    real_import = builtins.__import__

    def fail_on_playwright(name, *a, **kw):
        if name.startswith("playwright"):
            raise AssertionError("raw mode must never import playwright")
        return real_import(name, *a, **kw)

    fake = _fake_spider_with_start_page(2, 0, "<html><body>hi</body></html>")
    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    monkeypatch.setattr(builtins, "__import__", fail_on_playwright)

    handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))


# ── #242: resume must not depend on output.write_pages_jsonl ────────────────


def test_resume_reconstructs_pages_even_when_the_jsonl_export_is_disabled(tmp_path, monkeypatch):
    """The handler used to tie the only PageRecord sidecar spider.crawl_site can
    reload on resume to output.write_pages_jsonl, so an operator who disabled
    the human-readable export also -- silently -- disabled resume: a restored
    seen set suppressed the already-fetched page's URL without restoring its
    PageRecord, and the resumed run reported a smaller, non-partial corpus.
    Runs the real spider (not a fake) because the defect lived entirely in
    which path the handler wired into it, not in the spider's own reload
    logic, which already worked."""
    from dataclasses import dataclass

    @dataclass
    class _Response:
        status_code: int
        text: str
        headers: dict

        @property
        def content(self) -> bytes:
            return self.text.encode("utf-8")

    def make_fetcher(*, interrupt_b_once: bool):
        interrupted = False

        def fetch(url: str) -> _Response:
            nonlocal interrupted
            path = url.split("example.com", 1)[-1]
            if path == "/robots.txt":
                return _Response(200, "User-agent: *\nAllow: /", {"content-type": "text/plain"})
            if path == "/":
                return _Response(
                    200,
                    "<html><body><a href='/b'>B</a></body></html>",
                    {"content-type": "text/html"},
                )
            if path == "/b":
                if interrupt_b_once and not interrupted:
                    interrupted = True
                    raise KeyboardInterrupt
                return _Response(
                    200, "<html><body>second page</body></html>", {"content-type": "text/html"}
                )
            raise AssertionError(f"unexpected URL {url}")

        return fetch

    original_crawl_site = spider_mod.crawl_site

    def run(out_dir, fetch):
        config = out_dir.parent / f"{out_dir.name}.json"
        config.write_text(json.dumps({"output": {"dir": str(out_dir), "write_pages_jsonl": False}}))

        def injected(*args, **kwargs):
            return original_crawl_site(*args, fetcher=fetch, **kwargs)

        monkeypatch.setattr(spider_mod, "crawl_site", injected)
        return handlers.crawl_site(url="https://example.com/", config=str(config))

    out_dir = tmp_path / "resumed"

    interrupted = run(out_dir, make_fetcher(interrupt_b_once=True))
    assert interrupted["partial"] is True
    assert not (out_dir / "pages.jsonl").exists()

    resumed = run(out_dir, make_fetcher(interrupt_b_once=False))

    assert resumed["resumed"] is True
    assert resumed["partial"] is False
    assert resumed["urls_collected"] == 2
    audit = json.loads((out_dir / "audit.json").read_text())
    assert audit["summary"]["totals"]["urls_crawled"] == 2
    # The export stayed off throughout, as configured.
    assert not (out_dir / "pages.jsonl").exists()


def test_the_private_pages_sidecar_is_removed_once_the_crawl_finishes(tmp_path, monkeypatch):
    """Nothing is left to resume into once finish_reason is 'finished', so the
    private sidecar used only because write_pages_jsonl was off must not
    linger next to a finished run's output as a stale, hidden copy."""
    captured = {}

    def fake(*args, **kwargs):
        captured["out_path"] = kwargs["out_path"]
        result = SpiderResult()
        result.finish_reason = "finished"
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    config = tmp_path / "crawl.json"
    out_dir = tmp_path / "run"
    config.write_text(json.dumps({"output": {"dir": str(out_dir), "write_pages_jsonl": False}}))

    handlers.crawl_site(url="https://example.com/", config=str(config))

    assert captured["out_path"] == str(out_dir / ".pages_resume.jsonl")
    assert not (out_dir / ".pages_resume.jsonl").exists()


# ── #244: pages.jsonl must match audit.json after render escalation ─────────


def test_pages_jsonl_is_rewritten_to_match_audit_json_after_render_escalation(
    tmp_path, monkeypatch
):
    """The spider streams pages.jsonl during the static crawl, before handler-
    level render escalation exists to mutate result.pages in place. Without a
    rewrite afterward, pages.jsonl keeps static title/word-count/representation
    for a page whose in-memory record and audit.json both moved on to
    rendered -- breaking the per-page provenance guarantee the artifact makes."""
    out_dir = tmp_path / "run"
    pages = [
        PageRecord(url="https://example.com/", content_type="text/html", outlinks=1),
        PageRecord(url="https://example.com/app/1", content_type="text/html"),
    ]

    def fake_spider(*args, **kwargs):
        page_path = kwargs["out_path"]
        with open(page_path, "w", encoding="utf-8") as fh:
            for page in pages:
                fh.write(json.dumps(dataclasses.asdict(page)) + "\n")
        result = SpiderResult()
        result.pages = pages
        result.links = [LinkEdge("https://example.com/", "https://example.com/app/1", "app", False)]
        result.start_page_evidence = {"html": "<html><body><a href='/app/1'>app</a></body></html>"}
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake_spider)

    import seohead.checks.render as render_mod

    monkeypatch.setattr(
        render_mod,
        "render_check",
        lambda url, **kwargs: {"ok": True, "js_dependent": "/app/" in url, "empty_shell": None},
    )
    monkeypatch.setattr(
        render_mod,
        "render_document",
        lambda url, *a, **kw: {
            "ok": True,
            "final_url": url,
            "html": "<html><head><title>Rendered app</title></head><body>one two three</body></html>",
        },
    )

    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps(
            {
                "rendering": {
                    "mode": "js",
                    "escalation": {"sample_per_pattern": 1, "max_render_urls": 10},
                }
            }
        )
    )
    out = handlers.crawl_site(url="https://example.com/", config=str(config), out_dir=str(out_dir))

    assert out["render_escalation"]["render_requests"] == 1
    saved = [json.loads(line) for line in (out_dir / "pages.jsonl").read_text().splitlines()]
    saved_app = next(page for page in saved if page["url"].endswith("/app/1"))
    audit = json.loads((out_dir / "audit.json").read_text())
    audit_app = next(page for page in audit["pages"] if page["url"].endswith("/app/1"))

    assert pages[1].representation == "rendered"
    assert saved_app["representation"] == "rendered"
    assert audit_app["metrics"]["representation"] == "rendered"


# ── #246: partial native crawls withhold whole-graph link findings ──────────


def _partial_link_graph_result(*, partial: bool, finish_reason: str) -> SpiderResult:
    base = "https://example.test"

    def page(path: str, depth: int, outlinks: int) -> PageRecord:
        return PageRecord(
            url=f"{base}{path}",
            status_code=200,
            content_type="text/html",
            title=path or "/",
            h1="H",
            crawl_depth=depth,
            outlinks=outlinks,
        )

    core_paths = [f"/c{number}" for number in range(1, 13)]
    pages = [page("/", 0, len(core_paths) + 1), page("/lonely", 1, 0)]
    pages.extend(page(path, 1, len(core_paths)) for path in core_paths)

    links = [LinkEdge(f"{base}/", f"{base}{path}", path, False) for path in core_paths]
    links.append(LinkEdge(f"{base}/", f"{base}/lonely", "lonely", False))
    for source in core_paths:
        links.append(LinkEdge(f"{base}{source}", f"{base}/", "hub", False))
        links.extend(
            LinkEdge(f"{base}{source}", f"{base}{target}", target, False)
            for target in core_paths
            if target != source
        )

    result = SpiderResult(
        pages=pages,
        links=links,
        partial=partial,
        finish_reason=finish_reason,
        start_page_evidence={"html": "<html><body>measured static page</body></html>"},
    )
    if partial:
        result.stopped_reason = "url limit reached before the frontier was exhausted"
    return result


def test_low_link_score_is_withheld_on_a_partial_native_crawl(monkeypatch, tmp_path):
    """LOW_LINK_SCORE, like the three checks beside it in
    aggregate.GRAPH_WIDE_FINDING_CHECKS, is a whole-graph verdict: a URL-limit
    crawl's unfetched frontier could still hold the edge that would clear
    /lonely, so the finding must be withheld and named in checks_skipped
    rather than published against an admittedly incomplete graph."""
    monkeypatch.setattr(
        spider_mod,
        "crawl_site",
        lambda *a, **kw: _partial_link_graph_result(partial=True, finish_reason="url_limit"),
    )
    out = handlers.crawl_site(url="https://example.test/", out_dir=str(tmp_path / "partial"))
    assert out["partial"] is True
    assert out["summary"]["by_check"].get("LOW_LINK_SCORE", 0) == 0

    monkeypatch.setattr(
        spider_mod,
        "crawl_site",
        lambda *a, **kw: _partial_link_graph_result(partial=False, finish_reason="finished"),
    )
    complete = handlers.crawl_site(url="https://example.test/", out_dir=str(tmp_path / "complete"))
    assert complete["partial"] is False
    assert complete["summary"]["by_check"].get("LOW_LINK_SCORE") == 1


def test_low_link_score_withholding_is_recorded_not_silent(tmp_path, monkeypatch):
    """Absent from the findings is not the whole acceptance criterion: #246
    requires the withheld check land in checks_skipped with a partial-graph
    reason. ``aggregate._withhold_graph_wide_findings`` used to call
    ``ctx.skip()`` on a check that had already fired -- ``skip()`` no-ops on
    an id already in ``_fired_ids`` by design (see ``ctx.retract``'s own
    docstring) -- so the finding vanished from ``issues`` but never reached
    ``ctx.skipped`` either, reading as "silent" (never invoked) exactly like
    a check nobody ran. That is indistinguishable from the very confusion
    this withholding pass exists to prevent."""
    monkeypatch.setattr(
        spider_mod,
        "crawl_site",
        lambda *a, **kw: _partial_link_graph_result(partial=True, finish_reason="url_limit"),
    )
    handlers.crawl_site(url="https://example.test/", out_dir=str(tmp_path))
    audit = json.loads((tmp_path / "audit.json").read_text())

    skipped_ids = {s["id"] for s in audit["run"]["checks_skipped"]}
    assert "LOW_LINK_SCORE" in skipped_ids, "withheld finding must be named in checks_skipped"
    reason = next(
        s["reason"] for s in audit["run"]["checks_skipped"] if s["id"] == "LOW_LINK_SCORE"
    )
    assert "partial" in reason

    # Negative control: a check that never ran at all (never fired, never
    # withheld) is the one category that legitimately reads as silent -- the
    # bug's signature was LOW_LINK_SCORE joining that bucket by accident.
    assert "LOW_LINK_SCORE" not in audit["summary"]["check_coverage"]["checks_silent_ids"]


def test_inlink_boilerplate_only_is_withheld_on_a_partial_native_crawl(tmp_path, monkeypatch):
    """INLINK_BOILERPLATE_ONLY is a universal claim -- 'never linked from body
    content' -- computed by crawl_site itself rather than through
    seohead.sf.core.aggregate, so it needs its own partial-crawl guard rather
    than joining GRAPH_WIDE_FINDING_CHECKS. A url-limit crawl that only saw a
    footer link to /target must not publish the finding; the same graph on a
    finished crawl must."""
    base = "https://example.test"

    def page(path: str, depth: int) -> PageRecord:
        return PageRecord(
            url=f"{base}{path}",
            status_code=200,
            content_type="text/html",
            title=path or "/",
            h1="H",
            crawl_depth=depth,
            outlinks=1,
        )

    def result(*, partial: bool, finish_reason: str) -> SpiderResult:
        pages = [page("/", 0), page("/target", 1), page("/context", 1)]
        links = [LinkEdge(f"{base}/", f"{base}/target", "target", False, "footer")]
        return SpiderResult(
            pages=pages,
            links=links,
            partial=partial,
            finish_reason=finish_reason,
            start_page_evidence={"html": "<html><body>measured static page</body></html>"},
        )

    config = tmp_path / "crawl.json"
    config.write_text(json.dumps({"link_position": {"classify": True}}))

    monkeypatch.setattr(
        spider_mod, "crawl_site", lambda *a, **kw: result(partial=True, finish_reason="url_limit")
    )
    partial_out = handlers.crawl_site(
        url=f"{base}/", config=str(config), out_dir=str(tmp_path / "partial")
    )
    assert partial_out["summary"]["by_check"].get("INLINK_BOILERPLATE_ONLY", 0) == 0
    assert f"{base}/target" in partial_out["link_position"]["pages_boilerplate_only"]

    monkeypatch.setattr(
        spider_mod, "crawl_site", lambda *a, **kw: result(partial=False, finish_reason="finished")
    )
    complete_out = handlers.crawl_site(
        url=f"{base}/", config=str(config), out_dir=str(tmp_path / "complete")
    )
    assert complete_out["summary"]["by_check"].get("INLINK_BOILERPLATE_ONLY") == 1


def test_a_native_crawl_writes_the_backlog_beside_its_audit(monkeypatch, tmp_path):
    """A crawl done without Screaming Frog produced findings and no list of what to do
    about them. build_tasks has always accepted an audit document and crawl_site has
    always produced one; the two were simply never joined — the fourth instance in this
    repository of a module written and left unreachable (#128, #154, #165, #226)."""
    import json

    import seohead.crawl.spider as spider_mod
    from seohead.crawl.spider import SpiderResult

    result = SpiderResult()
    result.pages = [
        PageRecord(url="https://example.com/", status_code=200, content_type="text/html")
    ]
    monkeypatch.setattr(spider_mod, "crawl_site", lambda *a, **kw: result)

    out = tmp_path / "run"
    handlers.crawl_site(url="https://example.com/", out_dir=str(out))

    assert (out / "audit.json").is_file()
    assert (out / "tasks.json").is_file(), "the backlog must land beside the audit"
    assert (out / "tasks.md").is_file()

    backlog = json.loads((out / "tasks.json").read_text(encoding="utf-8"))
    assert backlog["source"], "the backlog names the run it came from"
    assert "# Audit Tasks" in (out / "tasks.md").read_text(encoding="utf-8")


def test_the_backlog_can_be_turned_off(monkeypatch, tmp_path):
    """It is a written artefact, not a finding, so an operator who does not want the two
    extra files can say so — and the audit is unaffected either way."""
    import seohead.crawl.spider as spider_mod
    from seohead.crawl.spider import SpiderResult

    result = SpiderResult()
    result.pages = [
        PageRecord(url="https://example.com/", status_code=200, content_type="text/html")
    ]
    monkeypatch.setattr(spider_mod, "crawl_site", lambda *a, **kw: result)

    config = tmp_path / "crawl.json"
    config.write_text('{"output": {"write_tasks": false}}', encoding="utf-8")
    out = tmp_path / "run"
    handlers.crawl_site(url="https://example.com/", out_dir=str(out), config=str(config))

    assert (out / "audit.json").is_file()
    assert not (out / "tasks.json").exists()
    assert not (out / "tasks.md").exists()


def test_same_origin_blank_link_is_absent_from_unsafe_cross_origin_summary(monkeypatch, tmp_path):
    """Issue #336: a same-origin target="_blank" link with no rel token is not a security
    finding, so it must not reach summary.by_check for either CLI or MCP's shared handler."""
    edge = LinkEdge(
        source="https://example.test/",
        destination="https://example.test/account",
        anchor="account",
        nofollow=False,
        target="_blank",
    )
    monkeypatch.setattr(spider_mod, "crawl_site", lambda *a, **kw: SpiderResult(links=[edge]))

    config = tmp_path / "crawl.json"
    config.write_text(json.dumps({"link_attributes": {"capture": True}}), encoding="utf-8")

    audit = handlers.crawl_site(
        url="https://example.test/", config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert audit["summary"]["by_check"].get("UNSAFE_CROSS_ORIGIN_LINK", 0) == 0


def test_normal_spider_discovery_names_the_directive_policy(monkeypatch, tmp_path):
    """Issue #332: the reference (docs/TOOL_REFERENCE.md) promises
    ``discovery.directive_policy`` for crawl-site's own result, not only for list mode
    -- a robots-blocked count with no stated policy is not self-explanatory."""

    def fake(*args, **kwargs):
        result = SpiderResult()
        result.robots_blocked = ["https://example.com/private/"]
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)

    out = handlers.crawl_site(
        url="https://example.com/", robots="report_only", out_dir=str(tmp_path)
    )

    assert out["discovery"]["mode"] == "spider"
    assert out["discovery"]["directive_policy"] == "report_only"
    assert out["discovery"]["robots_blocked"] == 1


def test_list_mode_directive_policy_still_matches_spider_mode(monkeypatch, tmp_path):
    """Negative control: list mode already reported this field (the bug was the spider
    branch omitting it) -- it must keep reporting the same value, unchanged."""
    import seohead.crawl.collect as collect_mod

    def fake_collect_urls(*args, **kwargs):
        result = SpiderResult()
        result.robots_blocked = ["https://example.com/private/"]
        return result

    monkeypatch.setattr(collect_mod, "collect_urls", fake_collect_urls)

    out = handlers.crawl_site(
        urls=["https://example.com/"], robots="report_only", out_dir=str(tmp_path)
    )

    assert out["discovery"]["mode"] == "list"
    assert out["discovery"]["directive_policy"] == "report_only"
    assert out["discovery"]["robots_blocked"] == 1


# --- scope.segments (#358) --------------------------------------------------


def test_a_scoped_crawl_names_its_segments_only_in_the_run_output(tmp_path, monkeypatch):
    """Acceptance criterion: a crawl scoped to one segment says so in its own run
    output, not leave it to be inferred from which URLs happen to be missing."""
    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps(
            {
                "scope": {
                    "segments": [{"name": "blog", "prefix": "/blog/"}],
                    "segments_only": ["blog"],
                }
            }
        )
    )
    monkeypatch.setattr(spider_mod, "crawl_site", lambda *a, **kw: SpiderResult())

    out = handlers.crawl_site(
        url="https://example.com/", config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert out["discovery"]["segments_only"] == ["blog"]


def test_an_unscoped_crawl_does_not_mention_segments_only(monkeypatch, tmp_path):
    monkeypatch.setattr(spider_mod, "crawl_site", lambda *a, **kw: SpiderResult())

    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))

    assert "segments_only" not in out["discovery"]


def test_segments_summary_reports_page_and_issue_counts_per_segment(tmp_path, monkeypatch):
    """Acceptance criterion: the audit reports page and issue counts per segment,
    and every URL lands in exactly one bucket."""
    config = tmp_path / "crawl.json"
    config.write_text(json.dumps({"scope": {"segments": [{"name": "blog", "prefix": "/blog/"}]}}))

    def fake(*args, **kwargs):
        result = SpiderResult()
        result.pages = [
            PageRecord(
                url="https://example.com/blog/post", status_code=200, content_type="text/html"
            ),
            PageRecord(
                url="https://example.com/shop/item", status_code=200, content_type="text/html"
            ),
        ]
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)

    out = handlers.crawl_site(
        url="https://example.com/", config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert out["segments"]["blog"]["pages"] == 1
    assert out["segments"]["default"]["pages"] == 1
    # Neither page carries a title -- TITLE_MISSING fires on both, each landing on
    # its own page's segment rather than being pooled into one undifferentiated count.
    assert out["segments"]["blog"]["issues"] >= 1
    assert out["segments"]["default"]["issues"] >= 1


def test_analysis_segments_use_post_crawl_fields_and_dependencies(tmp_path, monkeypatch):
    """#21 segments are analysis rules, separate from discovery scope rules."""
    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps(
            {
                "analysis": {
                    "segments": [
                        {
                            "name": "broken",
                            "rules": [{"op": "eq", "field": "status_code", "value": 404}],
                        },
                        {
                            "name": "broken-pages",
                            "rules": [{"op": "segment", "value": "broken"}],
                        },
                    ]
                }
            }
        )
    )

    def fake(*args, **kwargs):
        result = SpiderResult()
        result.pages = [
            PageRecord(url="https://example.com/missing", status_code=404),
            PageRecord(url="https://example.com/ok", status_code=200),
        ]
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    out = handlers.crawl_site(
        url="https://example.com/", config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert out["segments"]["broken"]["pages"] == 1
    assert out["segments"]["broken-pages"]["pages"] == 0


def test_analysis_segment_cycle_is_refused_before_a_crawl_starts(tmp_path, monkeypatch):
    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps(
            {
                "analysis": {
                    "segments": [
                        {"name": "a", "rules": [{"op": "segment", "value": "b"}]},
                        {"name": "b", "rules": [{"op": "segment", "value": "a"}]},
                    ]
                }
            }
        )
    )
    monkeypatch.setattr(
        spider_mod,
        "crawl_site",
        lambda *args, **kwargs: pytest.fail("cycle validation must run before collection"),
    )

    with pytest.raises(ValueError, match="circular segment dependency"):
        handlers.crawl_site(url="https://example.com/", config=str(config))


def test_a_no_target_issue_still_counts_in_the_segment_sum(tmp_path, monkeypatch):
    """Acceptance criterion (#441): an audit-wide finding with no single
    ``target_url`` (e.g. TITLE_TEMPLATED) still lands somewhere, so
    ``sum(segments[*].issues) == len(issues)`` never silently breaks."""

    class FakePage:
        def __init__(self, url):
            self.url = url

    pages = [FakePage("https://example.com/blog/a"), FakePage("https://example.com/shop/a")]
    issues = [
        {"check": "TITLE_MISSING", "target_url": "https://example.com/blog/a"},
        {"check": "TITLE_TEMPLATED", "target_url": None},
    ]

    segments = handlers._segment_counts(
        pages, issues, {"segments": [{"name": "blog", "prefix": "/blog/"}]}
    )

    assert sum(v["issues"] for v in segments.values()) == len(issues)
    assert segments["default"]["issues"] == 1
    assert segments["blog"]["issues"] == 1


def test_a_target_less_issue_does_not_appear_when_every_target_is_set(monkeypatch):
    """Negative control (#441): an audit where every issue has a target keeps
    behaving exactly as before -- the sums already matched in that case."""

    class FakePage:
        def __init__(self, url):
            self.url = url

    pages = [FakePage("https://example.com/blog/a")]
    issues = [{"check": "TITLE_MISSING", "target_url": "https://example.com/blog/a"}]

    segments = handlers._segment_counts(
        pages, issues, {"segments": [{"name": "blog", "prefix": "/blog/"}]}
    )

    assert sum(v["issues"] for v in segments.values()) == len(issues) == 1
    assert segments["blog"]["issues"] == 1
    assert "default" not in segments or segments["default"]["issues"] == 0


def test_segment_counting_is_wired_through_the_sf_core_segments_engine():
    """Acceptance criterion (#456): the tested segment engine is a real
    caller in the handlers path, not only imported by its own test."""
    import inspect

    source = inspect.getsource(handlers._segment_counts)
    assert "sf.core.segments" in source
    assert "assign_segments" in source


def test_without_declared_segments_no_segments_summary_is_reported(monkeypatch, tmp_path):
    """A plain crawl that never opted into #358 gets an unchanged, empty summary --
    not a single 'default' bucket holding everything, which would just be noise."""

    def fake(*args, **kwargs):
        result = SpiderResult()
        result.pages = [
            PageRecord(url="https://example.com/", status_code=200, content_type="text/html")
        ]
        return result

    monkeypatch.setattr(spider_mod, "crawl_site", fake)

    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))

    assert out["segments"] == {}


def test_list_mode_inside_a_project_defaults_to_the_project_scans_dir(tmp_path, monkeypatch):
    from seohead.projects.workspace import create_project

    root = tmp_path / "proj"
    create_project(root, "https://owner.example.test/")
    captured = {}

    def fake(urls, **kwargs):
        captured.update(kwargs)
        return {"ok": True}

    monkeypatch.setattr("seohead.mcp.scan_handlers.crawl_list_scan", fake)
    handlers.crawl_site(project=str(root), urls=["https://example.com/a"], approve_large_crawl=True)
    assert captured["scan_out"].startswith(str(root / "scans"))


def test_list_mode_without_a_project_still_needs_an_explicit_output(tmp_path):
    with pytest.raises(ValueError, match="list mode has no default SQLite artifact"):
        handlers.crawl_site(urls=["https://example.com/a"])


def test_list_mode_inside_a_project_registers_an_observer_run(tmp_path, monkeypatch):
    from seohead.projects import run_observation
    from seohead.projects.workspace import create_project

    root = tmp_path / "proj"
    create_project(root, "https://owner.example.test/")

    def fake(urls, **kwargs):
        return {"partial": False, "finish_reason": "finished", "audit_available": True}

    monkeypatch.setattr("seohead.mcp.scan_handlers.crawl_list_scan", fake)
    out = handlers.crawl_site(
        project=str(root), urls=["https://example.com/a"], approve_large_crawl=True
    )

    run = run_observation.status(root)["items"][0]
    assert out["observer_run_id"] == run["id"]
    assert run["kind"] == "native"
    assert run["collector"]["mode"] == "list"
    assert run["state"] == "finished"
    assert run["artifact"].startswith("scans/")


def test_list_mode_observer_run_is_failed_when_the_collector_raises(tmp_path, monkeypatch):
    from seohead.projects import run_observation
    from seohead.projects.workspace import create_project

    root = tmp_path / "proj"
    create_project(root, "https://owner.example.test/")

    def boom(urls, **kwargs):
        raise RuntimeError("collector down")

    monkeypatch.setattr("seohead.mcp.scan_handlers.crawl_list_scan", boom)
    with pytest.raises(RuntimeError):
        handlers.crawl_site(
            project=str(root), urls=["https://example.com/a"], approve_large_crawl=True
        )

    run = run_observation.status(root)["items"][0]
    assert run["state"] == "failed"
    assert run["finish_reason"] == "RuntimeError"


def test_crawl_site_result_reports_the_rate_and_warns_above_two(monkeypatch, tmp_path):
    monkeypatch.setattr(spider_mod, "crawl_site", lambda *a, **k: SpiderResult())

    fast = handlers.crawl_site(
        url="https://example.com/",
        out_dir=str(tmp_path),
        overrides={"speed.min_delay_seconds": 0.25},
    )
    assert fast["effective_max_requests_per_second"] == pytest.approx(4.0)
    assert fast["warnings"] and "above 2 req/s" in fast["warnings"][0]

    polite = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path))
    assert polite["effective_max_requests_per_second"] == pytest.approx(2.0)
    assert polite["warnings"] == []

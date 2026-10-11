"""Every setting #63 found "read by nothing" must change what a crawl does.

Same discipline as ``tests/test_crawl_scope.py``: one test proves the
configured value changes the outcome, one proves the default's own behaviour
still holds. No network — every response is a fake.

Settings already covered elsewhere by key name rather than by dotted path
(the coverage canary below greps for the dotted path literally):
``scope.exclude_hosts``, ``scope.include_patterns`` -> tests/test_crawl_scope.py.
``limits.max_crawl_seconds`` ->
tests/test_crawl_resume.py::test_max_seconds_stops_the_crawl_with_a_duration_finish_reason
(wired by the resumable-crawl work, not by this file).
``analysis.segments`` ->
tests/test_handlers_crawl_site.py::test_analysis_segments_use_post_crawl_fields_and_dependencies
(wired by the segments work, #21).
``discovery.resolve_canonical_destination`` ->
tests/test_crawl_list_mode.py (the canonical-chain-walk tests, #21).
"""

from __future__ import annotations

import json

from seohead.crawl.collect import CrawlResult, collect_urls, fetch_one
from seohead.crawl.settings import DEFAULTS, _flatten
from seohead.crawl.spider import SpiderResult, crawl_site, normalize_get_parameters
from seohead.crawl.throttle import MAX_DELAY_S, Throttle
from tests._crawl_fakes import FakeResponse, _fetcher, page


def _crawl(mapping, **kw):
    kw.setdefault("sleeper", lambda _s: None)
    kw.setdefault("min_delay", 0)
    return crawl_site("https://example.com/", fetcher=_fetcher(mapping), **kw)


def _fetched(result, url: str) -> bool:
    return any(p.url == url for p in result.pages)


ROBOTS_OK = FakeResponse("User-agent: *\nDisallow:\n", headers={"content-type": "text/plain"})


# ── limits.max_response_bytes ────────────────────────────────────────────────


def test_max_response_bytes_configured_skips_parsing_a_large_page():
    big = "<html><head><title>t</title></head><body>" + ("x" * 200) + "</body></html>"
    record, parsed = fetch_one(
        "https://example.com/", fetcher=lambda u: FakeResponse(big), max_response_bytes=50
    )
    assert parsed is None
    assert record.error == "response too large to parse"
    assert record.title == ""


def test_default_max_response_bytes_parses_a_normal_page():
    body = "<html><head><title>t</title></head><body><h1>t</h1></body></html>"
    record, parsed = fetch_one("https://example.com/", fetcher=lambda u: FakeResponse(body))
    assert parsed is not None
    assert record.title == "t"


# ── speed.max_delay_seconds ──────────────────────────────────────────────────


def test_max_delay_seconds_configured_caps_the_backoff_lower():
    t = Throttle(min_delay=0, max_delay=2.0)
    for _ in range(4):
        t.record_timeout()
    assert t.delay == 2.0


def test_default_max_delay_seconds_allows_a_much_higher_backoff():
    t = Throttle(min_delay=0)
    for _ in range(4):
        t.record_timeout()
    assert t.delay == MAX_DELAY_S


# ── robots.user_agent_token ──────────────────────────────────────────────────

SITE_TOKEN = {
    "https://example.com/robots.txt": FakeResponse(
        "User-agent: SpecialBot\nDisallow: /private/\nUser-agent: *\nDisallow:\n",
        headers={"content-type": "text/plain"},
    ),
    "https://example.com/": page("/private/secret"),
    "https://example.com/private/secret": page(),
}


def test_robots_user_agent_token_configured_matches_its_own_group():
    result = _crawl(SITE_TOKEN, robots_token="SpecialBot")
    assert not _fetched(result, "https://example.com/private/secret")
    assert result.excluded.get("blocked_by_robots") == 1


def test_default_robots_user_agent_token_falls_back_to_the_wildcard_group():
    result = _crawl(SITE_TOKEN)
    assert _fetched(result, "https://example.com/private/secret")


# ── robots.unavailable_means_stop ────────────────────────────────────────────

SITE_ROBOTS_DOWN = {
    "https://example.com/robots.txt": TimeoutError("connection timed out"),
    "https://example.com/": page("/a"),
    "https://example.com/a": page(),
}


def test_default_unavailable_means_stop_stops_the_crawl():
    result = _crawl(SITE_ROBOTS_DOWN)
    assert result.finish_reason == "robots_unavailable"
    assert result.partial is True
    assert result.pages == []


def test_unavailable_means_stop_configured_false_continues_unrestricted():
    result = _crawl(SITE_ROBOTS_DOWN, unavailable_means_stop=False)
    assert _fetched(result, "https://example.com/")
    assert _fetched(result, "https://example.com/a")


# ── speed.stop_after_consecutive_timeouts ────────────────────────────────────

SITE_TIMEOUTS = {
    "https://example.com/robots.txt": ROBOTS_OK,
    "https://example.com/": page(*[f"/{c}" for c in "abcdefg"]),
    **{f"https://example.com/{c}": TimeoutError("read timed out") for c in "abcdefg"},
}


def test_stop_after_consecutive_timeouts_configured_stops_earlier():
    result = _crawl(SITE_TIMEOUTS, stop_after_consecutive_timeouts=2)
    assert result.finish_reason == "errors"
    assert result.partial is True
    assert len(result.pages) == 3  # start page + 2 timed-out children


def test_default_stop_after_consecutive_timeouts_allows_more_failures_first():
    result = _crawl(SITE_TIMEOUTS)
    assert result.finish_reason == "errors"
    assert len(result.pages) == 6  # start page + 5 timed-out children (default limit)


# ── limits.max_url_length ────────────────────────────────────────────────────

LONG_PATH = "/" + ("p" * 60)
SITE_URL_LENGTH = {
    "https://example.com/robots.txt": ROBOTS_OK,
    "https://example.com/": page("/short", LONG_PATH),
    "https://example.com/short": page(),
    f"https://example.com{LONG_PATH}": page(),
}


def test_max_url_length_configured_excludes_a_long_discovered_link():
    result = _crawl(SITE_URL_LENGTH, max_url_length=40)
    assert not _fetched(result, f"https://example.com{LONG_PATH}")
    assert _fetched(result, "https://example.com/short")
    assert result.excluded.get("url_too_long") == 1


def test_default_max_url_length_allows_a_normal_length_url():
    result = _crawl(SITE_URL_LENGTH)
    assert _fetched(result, f"https://example.com{LONG_PATH}")


def test_list_mode_max_url_length_configured_skips_a_long_url():
    short_url = "https://example.com/a"
    long_url = "https://example.com/" + ("b" * 3000)
    mapping = {short_url: FakeResponse("<html><head><title>t</title></head><body></body></html>")}
    result = collect_urls([short_url, long_url], fetcher=lambda u: mapping[u], max_url_length=50)
    assert [p.url for p in result.pages] == [short_url]


def test_default_list_mode_max_url_length_allows_a_normal_url():
    short_url = "https://example.com/a"
    mapping = {short_url: FakeResponse("<html><head><title>t</title></head><body></body></html>")}
    result = collect_urls([short_url], fetcher=lambda u: mapping[u])
    assert [p.url for p in result.pages] == [short_url]


# ── limits.max_query_variants_per_path ───────────────────────────────────────

SITE_QUERY_VARIANTS = {
    "https://example.com/robots.txt": ROBOTS_OK,
    "https://example.com/": page("/search?q=a", "/search?q=b", "/search?q=c", "/search?q=d"),
    "https://example.com/search?q=a": page(),
    "https://example.com/search?q=b": page(),
    "https://example.com/search?q=c": page(),
    "https://example.com/search?q=d": page(),
}


def _search_urls(result) -> list[str]:
    return sorted(p.url for p in result.pages if p.url.startswith("https://example.com/search"))


def test_max_query_variants_per_path_configured_caps_the_budget():
    result = _crawl(SITE_QUERY_VARIANTS, max_query_variants_per_path=2)
    assert len(_search_urls(result)) == 2
    assert result.excluded.get("query_variants_limit") == 2


def test_default_max_query_variants_per_path_allows_more_before_capping():
    result = _crawl(SITE_QUERY_VARIANTS)
    assert len(_search_urls(result)) == 4


def test_native_site_crawl_normalizes_common_tracking_parameters():
    assert normalize_get_parameters(
        "https://example.test/p?utm_source=x&Keep=%2F&FBCLID=y#fragment"
    ) == "https://example.test/p?Keep=%2F#fragment"
    responses = {
        "https://example.com/robots.txt": ROBOTS_OK,
        "https://example.com/?keep=1": page(
            "/page?utm_source=mail&keep=2", "/page?fbclid=click&keep=2"
        ),
        "https://example.com/page?keep=2": page(),
    }
    result = crawl_site(
        "https://example.com/?utm_campaign=spring&keep=1",
        fetcher=_fetcher(responses),
        sleeper=lambda _s: None,
        min_delay=0,
    )
    fetched = [page.url for page in result.pages]
    assert fetched.count("https://example.com/page?keep=2") == 1
    assert "https://example.com/?keep=1" in fetched


def test_tracking_parameter_normalization_can_be_disabled():
    responses = {
        "https://example.com/robots.txt": ROBOTS_OK,
        "https://example.com/": page("/page?utm_source=mail", "/page?fbclid=click"),
        "https://example.com/page?utm_source=mail": page(),
        "https://example.com/page?fbclid=click": page(),
    }
    result = crawl_site(
        "https://example.com/",
        fetcher=_fetcher(responses),
        sleeper=lambda _s: None,
        min_delay=0,
        normalize_tracking_parameters=False,
    )
    assert {page.url for page in result.pages} >= {
        "https://example.com/page?utm_source=mail",
        "https://example.com/page?fbclid=click",
    }


SITE_NOFOLLOW_QUERY_BUDGET = {
    "https://example.com/robots.txt": ROBOTS_OK,
    "https://example.com/": FakeResponse(
        '<a rel="nofollow" href="/catalog?source=ignored">ignored</a><a href="/next">next</a>'
    ),
    "https://example.com/next": FakeResponse('<a href="/catalog?source=real">real</a>'),
    "https://example.com/catalog?source=real": page(),
}


def test_a_rejected_nofollow_query_url_does_not_spend_the_query_budget():
    """#193: extra_rejection reserves a query-variant slot for a path the instant it
    runs, whether or not the URL is ever fetched. With the cap at one, a nofollow
    query link that is never going to be dispatched must not be the one that spends
    it -- a later, real query URL for the same path is still owed its slot."""
    result = _crawl(SITE_NOFOLLOW_QUERY_BUDGET, max_query_variants_per_path=1)
    assert _fetched(result, "https://example.com/catalog?source=real")
    assert result.excluded.get("query_variants_limit") is None
    assert result.excluded.get("nofollow") == 1


# ── http.retry_on_timeout ────────────────────────────────────────────────────


def _flaky_fetcher():
    calls = {"n": 0}

    def fetch(_url):
        calls["n"] += 1
        if calls["n"] == 1:
            raise TimeoutError("read timed out")
        return FakeResponse("<html><head><title>t</title></head><body></body></html>")

    fetch.calls = calls
    return fetch


def test_retry_on_timeout_configured_recovers_from_a_single_timeout():
    fetcher = _flaky_fetcher()
    record, parsed = fetch_one("https://example.com/", fetcher=fetcher, retry_on_timeout=1)
    assert record.error == ""
    assert parsed is not None
    assert fetcher.calls["n"] == 2


def test_default_retry_on_timeout_does_not_retry():
    fetcher = _flaky_fetcher()
    record, parsed = fetch_one("https://example.com/", fetcher=fetcher)
    assert "timed out" in record.error
    assert parsed is None
    assert fetcher.calls["n"] == 1


# ── discovery.follow_nofollow ─────────────────────────────────────────────────


def _page_with_nofollow(href: str) -> FakeResponse:
    body = f'<a href="{href}" rel="nofollow">link</a>'
    return FakeResponse(f"<html><head><title>t</title></head><body><h1>t</h1>{body}</body></html>")


SITE_NOFOLLOW = {
    "https://example.com/robots.txt": ROBOTS_OK,
    "https://example.com/": _page_with_nofollow("/nf"),
    "https://example.com/nf": page(),
}


def test_follow_nofollow_configured_true_enqueues_nofollow_links():
    result = _crawl(SITE_NOFOLLOW, follow_nofollow=True)
    assert _fetched(result, "https://example.com/nf")
    assert any(e.destination == "https://example.com/nf" and e.nofollow for e in result.links)


def test_default_follow_nofollow_excludes_nofollow_links():
    result = _crawl(SITE_NOFOLLOW)
    assert not _fetched(result, "https://example.com/nf")
    assert result.excluded.get("nofollow") == 1
    # "store" and "crawl" stay independent: the edge is recorded either way.
    assert any(e.destination == "https://example.com/nf" and e.nofollow for e in result.links)


# ── http.user_agent ───────────────────────────────────────────────────────────


class _FakeClient:
    def __init__(self, response: FakeResponse) -> None:
        self._response = response
        self.captured_headers: dict[str, str] | None = None

    def get(self, target, *, headers, extensions):
        self.captured_headers = headers
        return self._response


_PAGE_BODY = "<html><head><title>t</title></head><body></body></html>"


def test_user_agent_configured_is_sent_on_the_request(monkeypatch):
    import seohead.crawl.collect as collect_mod

    monkeypatch.setattr(collect_mod, "validate_url", lambda u: u)
    monkeypatch.setattr(collect_mod, "pinned_target", lambda u: (u, {}, {}))
    client = _FakeClient(FakeResponse(_PAGE_BODY))

    record, _ = collect_mod.fetch_one("https://example.com/", client=client, user_agent="MyBot/9.0")
    assert client.captured_headers["User-Agent"] == "MyBot/9.0"
    assert record.status_code == 200


def test_default_user_agent_falls_back_to_the_toolkits_identifiable_default(monkeypatch):
    import seohead.crawl.collect as collect_mod
    from seohead.recon.net import UA

    monkeypatch.setattr(collect_mod, "validate_url", lambda u: u)
    monkeypatch.setattr(collect_mod, "pinned_target", lambda u: (u, {}, {}))
    client = _FakeClient(FakeResponse(_PAGE_BODY))

    record, _ = collect_mod.fetch_one("https://example.com/", client=client)
    assert client.captured_headers["User-Agent"] == UA
    assert record.status_code == 200


# ── handler wiring: settings must reach the collector under the right names ─


def test_handler_threads_every_newly_wired_setting_into_the_spider(monkeypatch, tmp_path):
    import seohead.crawl.spider as spider_mod
    from seohead.mcp import handlers

    captured: dict = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return SpiderResult()

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps(
            {
                "limits": {
                    "max_response_bytes": 999,
                    "max_url_length": 123,
                    "max_query_variants_per_path": 3,
                },
                "http": {"retry_on_timeout": 2, "user_agent": "MyBot"},
                "robots": {"user_agent_token": "MyBotToken", "unavailable_means_stop": False},
                "speed": {"stop_after_consecutive_timeouts": 7, "max_delay_seconds": 12.0},
                "discovery": {"follow_nofollow": True},
            }
        )
    )

    handlers.crawl_site(
        url="https://example.com/", config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert captured["max_response_bytes"] == 999
    assert captured["max_url_length"] == 123
    assert captured["max_query_variants_per_path"] == 3
    assert captured["retry_on_timeout"] == 2
    assert captured["user_agent"] == "MyBot"
    assert captured["robots_token"] == "MyBotToken"
    assert captured["unavailable_means_stop"] is False
    assert captured["stop_after_consecutive_timeouts"] == 7
    assert captured["max_delay_seconds"] == 12.0
    assert captured["follow_nofollow"] is True


def test_handler_threads_every_newly_wired_setting_into_collect_urls(monkeypatch, tmp_path):
    import seohead.crawl.collect as collect_mod
    from seohead.mcp import handlers

    captured: dict = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return CrawlResult()

    monkeypatch.setattr(collect_mod, "collect_urls", fake)
    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps(
            {
                "limits": {"max_response_bytes": 999, "max_url_length": 123},
                "http": {"retry_on_timeout": 2, "user_agent": "MyBot"},
                "speed": {"stop_after_consecutive_timeouts": 7, "max_delay_seconds": 12.0},
            }
        )
    )

    handlers.crawl_site(
        urls=["https://example.com/a"], config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert captured["max_response_bytes"] == 999
    assert captured["max_url_length"] == 123
    assert captured["retry_on_timeout"] == 2
    assert captured["user_agent"] == "MyBot"
    assert captured["stop_after_consecutive_timeouts"] == 7
    assert captured["max_delay_seconds"] == 12.0


# ── sitemaps.auto_discover (wired, previously untested at the config level) ──


def test_sitemaps_auto_discover_configured_seeds_without_an_explicit_sitemap_arg(
    tmp_path, monkeypatch
):
    import seohead.checks.robots as robots_tool
    import seohead.checks.sitemap as sitemap_tool
    import seohead.crawl.spider as spider_mod
    from seohead.mcp import handlers

    monkeypatch.setattr(
        robots_tool,
        "check_robots",
        lambda url, **_kwargs: {"sitemaps": ["https://example.com/sitemap.xml"]},
    )
    monkeypatch.setattr(
        sitemap_tool,
        "crawl",
        lambda url, concurrency=3, **_kwargs: {"urls": [{"loc": "https://example.com/a"}]},
    )
    captured: dict = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return SpiderResult()

    monkeypatch.setattr(spider_mod, "crawl_site", fake)

    config = tmp_path / "crawl.json"
    config.write_text(json.dumps({"sitemaps": {"auto_discover": True}}))

    out = handlers.crawl_site(
        url="https://example.com/", config=str(config), out_dir=str(tmp_path / "legacy")
    )
    assert out["discovery"]["sitemap_url"] == "https://example.com/sitemap.xml"
    # The discovered sitemap's declared URLs actually reach the spider as seeds.
    assert captured["seed_urls"] == ["https://example.com/a"]


def test_default_sitemaps_auto_discover_does_not_seed(monkeypatch, tmp_path):
    from seohead.mcp import handlers

    monkeypatch.setattr("seohead.crawl.spider.crawl_site", lambda *a, **kw: SpiderResult())
    out = handlers.crawl_site(url="https://example.com/", out_dir=str(tmp_path / "legacy"))
    assert out["discovery"]["sitemap_url"] is None
    assert out["discovery"]["sitemap_seeded"] == 0


# ── coverage canary: a DEFAULTS path nobody tests is #63 recurring ───────────

# Known, separately-tracked gap that predates this fix and is not part of
# #63's enumerated list: validated, described, and classified, but still read
# by nothing in seohead/crawl. Flagged for follow-up rather than folded into
# this PR (canonical-chasing and cross-host crawling are new capabilities, not
# a hardcode-to-config move). Do not add a new setting to this set — wire it
# and point at its test above, or delete it.
# Empty, and it must stay that way: every DEFAULTS path is now read by something. The set is
# kept rather than deleted because it is the shape a future "validated but unwired" setting
# would have to be added to, and adding to it is meant to be an argued exception (#63, #91).
_KNOWN_UNWIRED_GAP: frozenset[str] = frozenset()


def test_every_default_path_is_exercised_by_a_test():
    """A DEFAULTS path no test ever names is exactly the #63 defect recurring.

    Checked by literal presence of the dotted path somewhere in ``tests/`` — a
    config override, an assertion, or a comment pointing at the test that
    covers it behaviourally elsewhere. ``_KNOWN_UNWIRED_GAP`` is the one
    permitted exemption; see its comment before adding to it.
    """
    import pathlib

    tests_dir = pathlib.Path(__file__).resolve().parent
    corpus = "\n".join(p.read_text(encoding="utf-8") for p in sorted(tests_dir.glob("*.py")))
    paths = set(_flatten(DEFAULTS)) - _KNOWN_UNWIRED_GAP
    missing = sorted(p for p in paths if p not in corpus)
    assert not missing, f"settings never referenced by any test: {missing}"


def test_the_known_gap_exemption_names_only_real_settings():
    """The exemption above must shrink, never grow, as settings get wired."""
    assert set(_flatten(DEFAULTS)) >= _KNOWN_UNWIRED_GAP


# ── http.headers ─────────────────────────────────────────────────────────────


def _header_recording_client():
    class Client:
        def __init__(self):
            self.sent: list[dict] = []

        def get(self, target, *, headers, extensions):
            self.sent.append(dict(headers))
            return FakeResponse("<html><head><title>t</title></head><body>x</body></html>")

    return Client()


def test_http_headers_configured_are_sent_on_every_request(monkeypatch):
    import seohead.crawl.collect as collect_mod

    monkeypatch.setattr(collect_mod, "validate_url", lambda u: u)
    monkeypatch.setattr(collect_mod, "pinned_target", lambda u: (u, {}, {}))
    client = _header_recording_client()

    fetch_one(
        "https://example.com/",
        client=client,
        extra_headers={"X-Audit": "seohead", "Accept-Language": "be"},
    )

    assert client.sent[0]["X-Audit"] == "seohead"
    assert client.sent[0]["Accept-Language"] == "be"


def test_default_http_headers_add_nothing_beyond_the_user_agent(monkeypatch):
    import seohead.crawl.collect as collect_mod

    monkeypatch.setattr(collect_mod, "validate_url", lambda u: u)
    monkeypatch.setattr(collect_mod, "pinned_target", lambda u: (u, {}, {}))
    client = _header_recording_client()

    fetch_one("https://example.com/", client=client)

    assert set(client.sent[0]) == {"User-Agent"}


# ── speed.adaptive ───────────────────────────────────────────────────────────


def test_adaptive_off_keeps_the_configured_delay_whatever_the_origin_does():
    throttle = Throttle(min_delay=0.5, max_delay=MAX_DELAY_S, max_concurrency=4, adaptive=False)

    throttle.record_response(latency_s=9.0, ok=True)
    throttle.record_timeout()
    throttle.record_server_error(429)

    assert throttle.delay == 0.5
    # The give-up counters are a separate mechanism and keep running: a non-adaptive crawl
    # still has to stop when the origin stops answering.
    assert throttle.timeouts == 1
    assert throttle.server_errors == 1


def test_adaptive_on_is_the_default_and_still_widens_on_a_slow_origin():
    throttle = Throttle(min_delay=0.5, max_delay=MAX_DELAY_S, max_concurrency=4)
    throttle.record_response(latency_s=9.0, ok=True)
    assert throttle.delay > 0.5


# ── discovery.hyperlinks.store / .crawl ──────────────────────────────────────


_TWO_PAGE_SITE = {
    "https://example.com/": page("/a"),
    "https://example.com/a": page(),
    "https://example.com/robots.txt": ROBOTS_OK,
}


def test_hyperlinks_crawl_off_records_the_edge_but_never_fetches_it():
    result = _crawl(_TWO_PAGE_SITE, crawl_hyperlinks=False)
    assert not _fetched(result, "https://example.com/a")
    assert [edge.destination for edge in result.links] == ["https://example.com/a"]
    assert result.excluded.get("hyperlink_discovery_off") == 1


def test_hyperlinks_store_off_drops_the_edge_but_still_fetches_the_page():
    result = _crawl(_TWO_PAGE_SITE, store_hyperlinks=False)
    assert _fetched(result, "https://example.com/a")
    assert result.links == []


def test_both_hyperlink_defaults_store_and_follow():
    result = _crawl(_TWO_PAGE_SITE)
    assert _fetched(result, "https://example.com/a")
    assert [edge.destination for edge in result.links] == ["https://example.com/a"]


# ── link_attributes.capture ──────────────────────────────────────────────────

_SITE_WITH_A_RICH_LINK = {
    "https://example.com/": FakeResponse(
        "<html><head><title>t</title></head><body>"
        '<a href="//cdn.example/x" target="_blank" rel="noopener">cdn</a>'
        "</body></html>"
    ),
    "https://example.com/robots.txt": ROBOTS_OK,
}


def test_capture_attributes_configured_records_rel_target_and_raw_href():
    result = _crawl(_SITE_WITH_A_RICH_LINK, capture_link_attributes=True)
    [edge] = result.links
    assert edge.rel == ("noopener",)
    assert edge.target == "_blank"
    assert edge.raw_href == "//cdn.example/x"


def test_default_capture_attributes_leaves_rel_target_and_raw_href_unmeasured():
    result = _crawl(_SITE_WITH_A_RICH_LINK)
    [edge] = result.links
    assert edge.rel == ()
    assert edge.target == ""
    assert edge.raw_href == ""
    assert edge.nofollow is False  # derived independently of capture_attributes


# ── discovery.external.store ─────────────────────────────────────────────────


_SITE_WITH_AN_OUTBOUND_LINK = {
    "https://example.com/": page("/a", "https://other.example/x"),
    "https://example.com/a": page(),
    "https://example.com/robots.txt": ROBOTS_OK,
}


def test_external_store_off_drops_the_off_host_edge_only():
    result = _crawl(_SITE_WITH_AN_OUTBOUND_LINK, store_external_links=False)
    destinations = [edge.destination for edge in result.links]
    assert destinations == ["https://example.com/a"]


def test_external_store_default_keeps_the_off_host_edge_without_fetching_it():
    result = _crawl(_SITE_WITH_AN_OUTBOUND_LINK)
    destinations = [edge.destination for edge in result.links]
    assert "https://other.example/x" in destinations
    assert not _fetched(result, "https://other.example/x")


# ── discovery.redirects.crawl ────────────────────────────────────────────────


_SITE_WITH_A_REDIRECT = {
    "https://example.com/": page("/old"),
    "https://example.com/old": FakeResponse(
        "", status_code=301, headers={"location": "https://example.com/new"}
    ),
    "https://example.com/new": page(),
    "https://example.com/robots.txt": ROBOTS_OK,
}


def test_redirects_crawl_off_never_follows_a_redirect_target():
    result = _crawl(_SITE_WITH_A_REDIRECT, crawl_redirects=False)
    assert _fetched(result, "https://example.com/old")
    assert not _fetched(result, "https://example.com/new")


def test_redirects_crawl_default_follows_the_target():
    result = _crawl(_SITE_WITH_A_REDIRECT)
    assert _fetched(result, "https://example.com/new")


# ── the handler threads all of them ──────────────────────────────────────────


def test_handler_threads_the_remaining_settings_into_the_spider(monkeypatch, tmp_path):
    """discovery.hyperlinks.store, discovery.hyperlinks.crawl, discovery.external.store,
    discovery.redirects.crawl, link_attributes.capture, http.headers and speed.adaptive
    (#91, #125)."""
    import seohead.crawl.spider as spider_mod
    from seohead.mcp import handlers

    captured: dict = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return SpiderResult()

    monkeypatch.setattr(spider_mod, "crawl_site", fake)
    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps(
            {
                "http": {"headers": {"X-Audit": "seohead"}},
                "speed": {"adaptive": False},
                "discovery": {
                    "hyperlinks": {"store": False, "crawl": False},
                    "external": {"store": False},
                    "redirects": {"crawl": False},
                },
                "link_attributes": {"capture": True},
            }
        )
    )

    handlers.crawl_site(
        url="https://example.com/", config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert captured["extra_request_headers"] == {"X-Audit": "seohead"}
    assert captured["adaptive"] is False
    assert captured["store_hyperlinks"] is False
    assert captured["crawl_hyperlinks"] is False
    assert captured["store_external_links"] is False
    assert captured["crawl_redirects"] is False
    assert captured["capture_link_attributes"] is True


def test_handler_threads_headers_and_adaptive_into_collect_urls(monkeypatch, tmp_path):
    import seohead.crawl.collect as collect_mod
    from seohead.mcp import handlers

    captured: dict = {}

    def fake(*args, **kwargs):
        captured.update(kwargs)
        return CrawlResult()

    monkeypatch.setattr(collect_mod, "collect_urls", fake)
    config = tmp_path / "crawl.json"
    config.write_text(
        json.dumps({"http": {"headers": {"X-Audit": "s"}}, "speed": {"adaptive": False}})
    )

    handlers.crawl_site(
        urls=["https://example.com/"], config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert captured["extra_request_headers"] == {"X-Audit": "s"}
    assert captured["adaptive"] is False


# ── discovery.resolve_redirect_destination ───────────────────────────────────


def test_resolve_redirect_destination_reaches_the_list_mode_collector(monkeypatch, tmp_path):
    """discovery.resolve_redirect_destination is list mode's own setting (#21), so it is
    threaded into collect_urls rather than the spider: a URL list never discovers links, and
    this is the one thing that makes it follow a redirect past its first hop anyway."""
    import json as _json

    import seohead.crawl.collect as collect_mod
    from seohead.crawl.collect import CrawlResult
    from seohead.mcp import handlers

    captured: dict = {}

    def fake(urls, **kwargs):
        captured.update(kwargs)
        return CrawlResult()

    monkeypatch.setattr(collect_mod, "collect_urls", fake)
    config = tmp_path / "crawl.json"
    config.write_text(
        _json.dumps({"discovery": {"resolve_redirect_destination": True}}), encoding="utf-8"
    )

    handlers.crawl_site(
        urls=["https://example.com/old"], config=str(config), out_dir=str(tmp_path / "legacy")
    )

    assert captured["resolve_redirect_destination"] is True


def test_resolve_redirect_destination_defaults_off():
    """Off unless asked: following a chain costs a request per hop, and a plain status check
    of a URL list does not need it."""
    import seohead.crawl.settings as settings_mod

    assert settings_mod.DEFAULTS["discovery"]["resolve_redirect_destination"] is False

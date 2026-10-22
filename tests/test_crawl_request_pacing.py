"""Every follow-up request uses the current shared dispatch budget."""

import httpx
import pytest

from seohead.crawl.collect import collect_urls
from seohead.crawl.settings import load
from seohead.crawl.spider import _DispatchGate, crawl_site
from seohead.crawl.sqlite_adapter import crawl_to_scan
from seohead.crawl.throttle import RequestBudgetExhausted, Throttle


def test_dispatch_gate_counts_and_refuses_total_http_attempts():
    gate = _DispatchGate(Throttle(), lambda _seconds: None, max_requests=2)

    gate.wait_turn()
    gate.wait_turn()

    with pytest.raises(RequestBudgetExhausted):
        gate.wait_turn()
    assert gate.requests_used == 2


def test_dispatch_gate_applies_a_new_timeout_penalty_to_the_next_turn():
    now = [0.0]
    throttle = Throttle(min_delay=1)
    gate = _DispatchGate(
        throttle, lambda seconds: now.__setitem__(0, now[0] + seconds), lambda: now[0]
    )
    gate.wait_turn()
    throttle.record_timeout()
    gate.wait_turn()
    assert now[0] == throttle.delay == 4


def test_list_retry_waits_for_the_increased_timeout_delay():
    now = [0.0]
    attempts = []

    def fetch(url):
        attempts.append(now[0])
        if len(attempts) == 1:
            raise httpx.ReadTimeout("timeout")
        return httpx.Response(200, text="<html>ok</html>", request=httpx.Request("GET", url))

    result = collect_urls(
        ["https://example.test/"],
        min_delay=1,
        retry_on_timeout=1,
        fetcher=fetch,
        sleeper=lambda seconds: now.__setitem__(0, now[0] + seconds),
        clock=lambda: now[0],
    )
    assert result.pages[0].status_code == 200
    assert attempts == [0, 4]


def test_sqlite_redirect_resolution_claims_a_turn_for_every_hop(tmp_path):
    now = [0.0]
    attempts = []

    def fetch(url):
        path = httpx.URL(url).path
        attempts.append((now[0], path))
        destination = {"/": "/hop", "/hop": "/final"}.get(path)
        return httpx.Response(
            301 if destination else 200,
            text="" if destination else "<html>done</html>",
            headers={"location": destination} if destination else {"content-type": "text/html"},
            request=httpx.Request("GET", url),
        )

    run = crawl_to_scan(
        "https://example.test/",
        scan_out=str(tmp_path / "redirect.sqlite"),
        settings=load(
            overrides={
                "speed.min_delay_seconds": 1,
                "speed.adaptive": False,
                "robots.policy": "ignore",
                "limits.max_urls": 1,
                "discovery.resolve_redirect_destination": True,
            }
        ),
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions={
            name: "test" for name in ("python", "sqlite", "httpx", "lxml", "beautifulsoup4")
        },
        fetcher=fetch,
        sleeper=lambda seconds: now.__setitem__(0, now[0] + seconds),
        clock=lambda: now[0],
    )
    assert run.pages == 1
    assert attempts == [(0, "/"), (1, "/hop"), (2, "/final")]


def test_one_gate_covers_seed_discovery_spider_and_post_audit_sitemap_rechecks(
    monkeypatch, tmp_path
):
    from seohead.mcp import handlers
    from seohead.sf.core import sitemap_coverage as coverage
    from tests.test_sitemap_coverage import _mini_ctx

    now = [0.0]
    calls = []
    throttle = Throttle(min_delay=1.0, adaptive=False)
    gate = _DispatchGate(
        throttle,
        lambda seconds: now.__setitem__(0, now[0] + seconds),
        lambda: now[0],
    )
    sitemap_url = "https://example.test/sitemap.xml"
    group = {
        "user_agents": ["*"],
        "allow": [],
        "disallow": [],
        "crawl_delay": None,
        "request_rate_delay": 3.0,
    }

    def seed_robots(_url, *, request_gate=None, **_kwargs):
        request_gate()
        calls.append((now[0], "seed robots"))
        return {"ok": True, "groups": [group], "sitemaps": [sitemap_url]}

    def seed_sitemap(_url, *, request_gate=None, **_kwargs):
        request_gate()
        calls.append((now[0], "seed sitemap"))
        return {"urls": [{"loc": "https://example.test/"}]}

    monkeypatch.setattr("seohead.checks.robots.check_robots", seed_robots)
    monkeypatch.setattr("seohead.checks.sitemap.crawl", seed_sitemap)
    seeded = handlers._seed_urls_from_sitemap(
        "https://example.test/",
        None,
        True,
        request_gate=gate.wait_turn,
        robots_token="*",
        throttle=throttle,
    )
    assert seeded["declared"] == ["https://example.test/"]
    assert throttle.min_delay == 3.0

    def spider_fetch(url):
        calls.append((now[0], "spider robots" if url.endswith("robots.txt") else "spider page"))
        if url.endswith("robots.txt"):
            return httpx.Response(
                200,
                text="User-agent: *\nRequest-rate: 1/3\n",
                headers={"content-type": "text/plain"},
            )
        return httpx.Response(200, text="<html><body>page</body></html>")

    result = crawl_site(
        "https://example.test/",
        max_urls=1,
        robots_policy="respect",
        fetcher=spider_fetch,
        min_delay=1.0,
        adaptive=False,
        dispatch_gate=gate,
        sleeper=lambda seconds: now.__setitem__(0, now[0] + seconds),
        clock=lambda: now[0],
    )
    assert result.crawl_delay_applied == 3.0

    def audit_handler(request):
        calls.append(
            (now[0], "audit robots" if request.url.path == "/robots.txt" else "audit sitemap")
        )
        body = (
            b"User-agent: *\nSitemap: https://example.test/sitemap.xml\n"
            if request.url.path == "/robots.txt"
            else b"<urlset><url><loc>https://example.test/</loc></url></urlset>"
        )
        return httpx.Response(
            200,
            stream=httpx.ByteStream(body),
            headers={
                "content-type": "text/plain"
                if request.url.path == "/robots.txt"
                else "application/xml"
            },
            request=request,
        )

    monkeypatch.setattr(coverage, "validate_url", lambda _url: None)
    monkeypatch.setattr(
        coverage,
        "http_client",
        lambda _timeout, **kwargs: (
            httpx.Client(
                transport=httpx.MockTransport(audit_handler),
                follow_redirects=kwargs["follow_redirects"],
                event_hooks=kwargs.get("event_hooks"),
            ),
            False,
        ),
    )
    coverage.run_sitemap(
        _mini_ctx(tmp_path, ["https://example.test/"]),
        sitemap_url=sitemap_url,
        request_gate=gate.wait_turn,
    )

    assert calls == [
        (0.0, "seed robots"),
        (3.0, "seed sitemap"),
        (6.0, "spider robots"),
        (9.0, "spider page"),
        (12.0, "audit robots"),
        (15.0, "audit sitemap"),
    ]


def test_relative_and_unusable_sitemap_lines_resolve_or_skip(monkeypatch):
    """robots.txt ``Sitemap: /sitemap.xml`` resolves against the site root; a line that
    still has no scheme is skipped with a record instead of aborting the crawl (#1552)."""
    fetched: list[str] = []

    def seed_robots(_url, **_kwargs):
        return {
            "ok": True,
            "groups": [],
            "sitemaps": ["/sitemap.xml", "https://example.test/other.xml", "ftp:/nope", "junk"],
        }

    def seed_sitemap(url, **_kwargs):
        fetched.append(url)
        return {"urls": [{"loc": f"https://example.test/from-{url.rsplit('/', 1)[1]}"}]}

    monkeypatch.setattr("seohead.checks.robots.check_robots", seed_robots)
    monkeypatch.setattr("seohead.checks.sitemap.crawl", seed_sitemap)
    from seohead.mcp import handlers

    seeded = handlers._seed_urls_from_sitemap("https://example.test/deep/page", None, True)
    assert seeded["sitemap_urls"] == [
        "https://example.test/sitemap.xml",
        "https://example.test/other.xml",
        "https://example.test/junk",  # bare relative words resolve against the root too
    ]
    assert seeded["skipped_sitemaps"] == ["ftp:/nope"]
    assert seeded["declared"][-1] == "https://example.test/from-junk"

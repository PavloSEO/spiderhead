"""Guardrails: which addresses are reachable, and how directives are obeyed."""

import httpx
import pytest

from seohead.checks.robots import (
    _rules_for,
    crawl_delay,
    is_allowed,
    parse_robots,
    politeness_delay,
    request_rate_delay,
)
from seohead.crawl.collect import collect_urls, fetch_one
from seohead.crawl.spider import crawl_site
from seohead.recon.net import _is_public_address


def test_aggregate_dispatch_gate_paces_robots_redirects_before_the_first_page(monkeypatch):
    """Robots bootstrap hops are HTTP attempts, not free pre-crawl setup (#14)."""
    import seohead.crawl.collect as collect
    import seohead.crawl.spider as spider

    now = [0.0]
    calls = []

    def handler(request):
        calls.append((now[0], request.url.path))
        if request.url.path == "/robots.txt":
            return httpx.Response(302, headers={"location": "/robots-1.txt"}, request=request)
        if request.url.path == "/robots-1.txt":
            return httpx.Response(302, headers={"location": "/robots-2.txt"}, request=request)
        if request.url.path == "/robots-2.txt":
            return httpx.Response(
                200,
                text="User-agent: *\nAllow: /\n",
                headers={"content-type": "text/plain"},
                request=request,
            )
        return httpx.Response(200, text="<html><title>page</title></html>", request=request)

    transport = httpx.MockTransport(handler)
    monkeypatch.setattr(
        spider,
        "http_client",
        lambda timeout, **kwargs: (
            httpx.Client(
                timeout=timeout, transport=transport, follow_redirects=kwargs["follow_redirects"]
            ),
            True,
        ),
    )
    monkeypatch.setattr(collect, "validate_url", lambda _url: None)
    monkeypatch.setattr(collect, "pinned_target", lambda url: (url, {}, {}))
    result = crawl_site(
        "https://example.test/",
        max_urls=1,
        min_delay=1.0,
        sleeper=lambda seconds: now.__setitem__(0, now[0] + seconds),
        clock=lambda: now[0],
    )
    assert len(result.pages) == 1
    assert calls == [
        (0.0, "/robots.txt"),
        (1.0, "/robots-1.txt"),
        (2.0, "/robots-2.txt"),
        (3.0, "/"),
    ]


class FakeResponse:
    def __init__(self, text="", status_code=200, ct="text/html"):
        self.text = text
        self.status_code = status_code
        self.headers = {"content-type": ct}


def page(*links, title="t"):
    body = "".join(f'<a href="{h}">x</a>' for h in links)
    return FakeResponse(
        f"<html><head><title>{title}</title></head><body><h1>H</h1>{body}</body></html>"
    )


# ── address guard ───────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "address,expected",
    [
        ("8.8.8.8", True),
        ("2001:4860:4860::8888", True),
        ("10.0.0.5", False),
        ("127.0.0.1", False),
        ("169.254.169.254", False),
        ("::ffff:127.0.0.1", False),
        # A non-public address wrapped in a globally-scoped one. Python's
        # is_global answers a question about the address family, not about
        # where the packet ends up.
        ("64:ff9b::7f00:1", False),  # NAT64 -> 127.0.0.1
        ("64:ff9b::a00:5", False),  # NAT64 -> 10.0.0.5
        ("64:ff9b::a9fe:a9fe", False),  # NAT64 -> cloud metadata
        ("64:ff9b:1::a9fe:a9fe", False),  # local-use NAT64 prefix
        ("2002:7f00:1::", False),  # 6to4 -> 127.0.0.1
        # The same wrappers around a genuinely public address stay allowed.
        ("64:ff9b::808:808", True),  # NAT64 -> 8.8.8.8
        ("2002:0808:0808::", True),  # 6to4 -> 8.8.8.8
    ],
)
def test_translated_addresses_are_judged_by_where_they_land(address, expected):
    assert _is_public_address(address) is expected


def test_a_malformed_address_is_not_public():
    assert _is_public_address("not-an-address") is False


# ── robots parsing ──────────────────────────────────────────────────────────

ROBOTS = """
User-agent: *
Crawl-delay: 2.5
Disallow: /private/

User-agent: Googlebot
Disallow: /nogoogle/

User-agent: SEOHEAD-Tools
Disallow: /ours/
"""


def test_crawl_delay_is_parsed():
    assert crawl_delay(parse_robots(ROBOTS)) == 2.5


def test_a_malformed_crawl_delay_is_ignored_rather_than_fatal():
    assert crawl_delay(parse_robots("User-agent: *\nCrawl-delay: soon\n")) is None


def test_a_comma_decimal_crawl_delay_is_understood():
    assert crawl_delay(parse_robots("User-agent: *\nCrawl-delay: 1,5\n")) == 1.5


def test_request_rate_supplies_a_conservative_minimum_interval():
    parsed = parse_robots("User-agent: *\nRequest-rate: 3/10\nCrawl-delay: 2\n")
    assert request_rate_delay(parsed) == pytest.approx(10 / 3)
    assert politeness_delay(parsed) == pytest.approx(10 / 3)


def test_repeated_matching_groups_keep_the_strictest_crawl_delay_floor():
    parsed = parse_robots(
        "User-agent: ExampleBot\nCrawl-delay: 1\nUser-agent: ExampleBot\nCrawl-delay: 10\n"
    )

    assert crawl_delay(parsed, "ExampleBot") == 10
    assert politeness_delay(parsed, "ExampleBot") == 10


@pytest.mark.parametrize("value", ["0/1", "1/0", "1.5/2", "1 / 2", "soon", "1/-2"])
def test_malformed_request_rate_is_ignored_without_affecting_crawl_delay(value):
    parsed = parse_robots(f"User-agent: *\nRequest-rate: {value}\nCrawl-delay: 2\n")
    assert request_rate_delay(parsed) is None
    assert politeness_delay(parsed) == 2


def test_the_most_specific_group_wins_not_the_last_one():
    parsed = parse_robots(ROBOTS)
    assert _rules_for(parsed, "SEOHEAD-Tools")["disallow"] == ["/ours/"]
    assert _rules_for(parsed, "Googlebot")["disallow"] == ["/nogoogle/"]


def test_a_token_matches_a_more_specific_agent_but_not_the_reverse():
    parsed = parse_robots(ROBOTS)
    assert _rules_for(parsed, "Googlebot-Image")["disallow"] == ["/nogoogle/"]


def test_a_token_appearing_mid_string_no_longer_claims_the_agent():
    """Substring matching let any group whose token appeared anywhere win.

    The function takes a product token, and a token must be a prefix of it —
    "bot" must not capture "SEOHEAD-Tools" merely by occurring inside a name.
    """
    parsed = parse_robots("User-agent: Tools\nDisallow: /\n\nUser-agent: *\nDisallow: /private/\n")
    assert _rules_for(parsed, "SEOHEAD-Tools")["disallow"] == ["/private/"]


def test_an_unknown_agent_falls_back_to_the_wildcard_group():
    assert _rules_for(parse_robots(ROBOTS), "SomeOtherBot")["disallow"] == ["/private/"]


def test_two_groups_for_the_same_agent_combine_instead_of_dropping_the_second():
    """RFC 9309 2.2.1: every group tied at the most specific match applies together.

    Keeping only the first such group (#237) meant a second, later ``Disallow``
    for a crawler already named earlier was silently unenforced."""
    parsed = parse_robots(
        "User-agent: ExampleBot\nDisallow: /private\n\nUser-agent: ExampleBot\nDisallow: /secret\n"
    )
    rules = _rules_for(parsed, "ExampleBot")
    assert rules["disallow"] == ["/private", "/secret"]


def test_combined_groups_still_lose_to_a_more_specific_single_group():
    parsed = parse_robots(
        "User-agent: *\nDisallow: /a\n\n"
        "User-agent: ExampleBot\nDisallow: /b\n\n"
        "User-agent: ExampleBot\nDisallow: /c\n"
    )
    assert _rules_for(parsed, "ExampleBot")["disallow"] == ["/b", "/c"]


def test_robots_check_advises_disallow_for_every_repeated_group_path():
    """The public is_allowed() surface, exercised the way robots-check calls it."""
    parsed = parse_robots(
        "User-agent: ExampleBot\nDisallow: /private\n\nUser-agent: ExampleBot\nDisallow: /secret\n"
    )
    assert is_allowed(parsed, "/private", "ExampleBot") is False
    assert is_allowed(parsed, "/secret", "ExampleBot") is False


# ── policy in the spider ────────────────────────────────────────────────────

SITE = {
    "https://example.com/robots.txt": FakeResponse(
        "User-agent: *\nCrawl-delay: 3\nDisallow: /private/\n", ct="text/plain"
    ),
    "https://example.com/": page("/a", "/private/secret"),
    "https://example.com/a": page(),
    "https://example.com/private/secret": page(),
}


def _crawl(policy, site=None, **kw):
    mapping = site or SITE
    return crawl_site(
        "https://example.com/",
        robots_policy=policy,
        min_delay=0,
        sleeper=lambda _s: None,
        fetcher=lambda u: mapping.get(u) or FakeResponse("", 404),
        **kw,
    )


def test_respect_does_not_fetch_disallowed_urls():
    result = _crawl("respect")
    assert "https://example.com/private/secret" not in {p.url for p in result.pages}
    assert result.excluded.get("blocked_by_robots") == 1


def test_report_only_crawls_the_url_and_still_reports_it_as_blocked():
    """Full coverage plus an inventory of what a compliant crawler would miss."""
    result = _crawl("report_only")
    assert "https://example.com/private/secret" in {p.url for p in result.pages}
    assert result.robots_blocked == ["https://example.com/private/secret"]
    assert "blocked_by_robots" not in result.excluded


def test_ignore_does_not_fetch_the_file_so_it_reports_nothing():
    result = _crawl("ignore")
    assert "https://example.com/private/secret" in {p.url for p in result.pages}
    assert result.robots_blocked == []
    assert "not fetched" in result.robots_note


def test_a_stated_crawl_delay_raises_the_floor():
    result = _crawl("respect")
    assert result.crawl_delay_applied == 3.0
    assert result.effective_delay >= 3.0


def test_a_configured_delay_higher_than_the_stated_one_is_kept():
    """The site's request is a floor on politeness, never a ceiling."""
    result = crawl_site(
        "https://example.com/",
        robots_policy="respect",
        min_delay=10.0,
        sleeper=lambda _s: None,
        fetcher=lambda u: SITE.get(u) or FakeResponse("", 404),
    )
    assert result.effective_delay >= 10.0


def test_a_delay_is_not_applied_when_robots_is_not_fetched():
    assert _crawl("ignore").crawl_delay_applied is None


def test_a_crawl_delay_above_max_delay_seconds_is_never_clamped_back_below_it():
    """Issue #150: a Crawl-delay above the crawl's own max_delay_seconds (left at
    its 60s default here) used to be honoured for exactly one request, then
    clamped down to max_delay -- below the value robots.txt asked for -- for the
    rest of the crawl, surviving neither a timeout nor a 5xx that should widen
    the delay further, never shrink it."""

    def fetcher(url):
        if url == "https://example.com/robots.txt":
            return FakeResponse("User-agent: *\nCrawl-delay: 100\n", ct="text/plain")
        if url == "https://example.com/":
            return page("/timeout-page", "/error-page", "/normal-page")
        if url == "https://example.com/timeout-page":
            raise TimeoutError("simulated read timeout")
        if url == "https://example.com/error-page":
            return FakeResponse("", 503)
        if url == "https://example.com/normal-page":
            return page()
        return FakeResponse("", 404)

    result = crawl_site(
        "https://example.com/",
        robots_policy="respect",
        min_delay=0,
        sleeper=lambda _s: None,
        fetcher=fetcher,
    )
    assert result.crawl_delay_applied == 100.0
    assert result.effective_delay >= 100.0


# ── pinned transport ────────────────────────────────────────────────────────


def test_a_request_is_pinned_to_the_address_that_was_vetted(monkeypatch):
    """Resolving twice leaves a window between the check and the connection."""
    from seohead.recon import net

    monkeypatch.setattr(
        net.socket,
        "getaddrinfo",
        lambda _host, port, **_kwargs: [
            (
                net.socket.AF_INET,
                net.socket.SOCK_STREAM,
                net.socket.IPPROTO_TCP,
                "",
                ("8.8.8.8", port),
            )
        ],
    )

    url, headers, extensions = net.pinned_target("https://example.com/path?a=1")
    assert headers["Host"] == "example.com"
    assert extensions["sni_hostname"] == "example.com"
    assert "example.com" not in url  # the connection goes to an address
    assert url.endswith("/path?a=1")


def test_pinning_preserves_a_non_default_port(monkeypatch):
    from seohead.recon import net

    monkeypatch.setattr(
        net.socket,
        "getaddrinfo",
        lambda _host, port, **_kwargs: [
            (
                net.socket.AF_INET,
                net.socket.SOCK_STREAM,
                net.socket.IPPROTO_TCP,
                "",
                ("8.8.8.8", port),
            )
        ],
    )

    url, headers, _ = net.pinned_target("https://example.com:8443/")
    assert headers["Host"] == "example.com:8443"
    assert url.endswith(":8443/")


def test_pinning_refuses_a_url_without_a_host():
    from seohead.recon.net import pinned_target

    with pytest.raises(ValueError, match="no host"):
        pinned_target("file:///etc/passwd")


def test_pinning_refuses_a_private_target():
    from seohead.recon.net import pinned_target

    with pytest.raises(ValueError):
        pinned_target("http://127.0.0.1:8080/")


def test_pinning_honours_the_named_host_allowlist_but_not_a_different_host(monkeypatch):
    """The connection path itself (not just validate_url) respects the scoped opt-in."""
    from seohead.recon import net

    records = [(net.socket.AF_INET, net.socket.SOCK_STREAM, 6, "", ("10.0.0.9", 443))]
    monkeypatch.setattr(net.socket, "getaddrinfo", lambda *_a, **_k: records)
    monkeypatch.delenv(net.PRIVATE_NETWORK_ENV, raising=False)
    monkeypatch.setenv(net.PRIVATE_HOST_ALLOWLIST_ENV, "staging.internal")

    url, headers, _ = net.pinned_target("https://staging.internal/")
    assert headers["Host"] == "staging.internal"
    assert "10.0.0.9" in url

    with pytest.raises(ValueError):
        net.pinned_target("https://other-internal.example/")


def test_pinning_prefers_a_vetted_ipv4_answer_for_an_ipv4_only_origin(monkeypatch):
    """Dual-stack DNS must not select an unreachable IPv6 loopback first."""
    from seohead.recon import net

    records = [
        (net.socket.AF_INET6, net.socket.SOCK_STREAM, 6, "", ("::1", 443, 0, 0)),
        (net.socket.AF_INET, net.socket.SOCK_STREAM, 6, "", ("127.0.0.1", 443)),
    ]
    monkeypatch.setattr(net.socket, "getaddrinfo", lambda *_args, **_kwargs: records)
    monkeypatch.setenv(net.PRIVATE_HOST_ALLOWLIST_ENV, "crawl.localhost")

    url, headers, extensions = net.pinned_target("https://crawl.localhost/")

    assert url.startswith("https://127.0.0.1/")
    assert headers == {"Host": "crawl.localhost"}
    assert extensions == {"sni_hostname": "crawl.localhost"}


def test_named_loopback_allowlist_survives_pre_pinned_collection(monkeypatch):
    """A vetted ``crawl.localhost`` connection must not become bare loopback.

    The collector intentionally connects to a literal address after validating
    the hostname. The request hook must preserve that host identity, while a
    forged different host remains blocked.
    """
    import socket
    import threading
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    from seohead.recon import net

    seen_hosts = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            seen_hosts.append(self.headers.get("Host"))
            number = int(self.path.strip("/") or "0")
            link = "" if number >= 2 else f'<a href="/{number + 1}">next</a>'
            body = f"<html><head><title>{number}</title></head><body>{link}</body></html>".encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    real_getaddrinfo = socket.getaddrinfo

    def resolve(host, port, *args, **kwargs):
        if host == "crawl.localhost":
            return real_getaddrinfo("127.0.0.1", port, *args, **kwargs)
        return real_getaddrinfo(host, port, *args, **kwargs)

    monkeypatch.setattr(net.socket, "getaddrinfo", resolve)
    monkeypatch.setenv(net.PRIVATE_HOST_ALLOWLIST_ENV, "crawl.localhost")
    monkeypatch.delenv(net.PRIVATE_NETWORK_ENV, raising=False)
    try:
        result = crawl_site(
            f"http://crawl.localhost:{server.server_port}/0",
            max_urls=3,
            min_delay=0,
            robots_policy="ignore",
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join()

    assert [page.status_code for page in result.pages] == [200, 200, 200]
    assert [page.url for page in result.pages] == [
        f"http://crawl.localhost:{server.server_port}/0",
        f"http://crawl.localhost:{server.server_port}/1",
        f"http://crawl.localhost:{server.server_port}/2",
    ]
    assert seen_hosts == [f"crawl.localhost:{server.server_port}"] * 3

    client, _ = net.http_client(5)
    forged = httpx.Request(
        "GET",
        f"http://127.0.0.1:{server.server_port}/",
        headers={"Host": f"other.localhost:{server.server_port}"},
        extensions={"sni_hostname": "other.localhost"},
    )
    with client, pytest.raises(ValueError, match="private or non-public network target blocked"):
        client.send(forged)


# ── http_client()'s pinning transport (#142) ─────────────────────────────────
#
# pinned_target() alone only protects a caller disciplined enough to call it.
# http_client() is the one function every tool actually calls, so the fix has
# to live in the client it hands back: a transport that pins the connection to
# the address it resolved itself, on every hop, rather than handing httpcore a
# hostname it would resolve a second time on its own.


def test_a_rebinding_resolver_cannot_move_a_plain_http_client_request():
    """The exact shape from issue #142: a resolver that answers the check with
    a public address and every later lookup for the same host with a loopback
    one. Before the fix, httpcore's own connect did that later lookup and
    nothing re-checked its answer. After the fix, the transport's own
    resolution is what connects, so this must always fail closed rather than
    silently reach the loopback address."""
    import socket

    from seohead.recon import net

    real_getaddrinfo = socket.getaddrinfo
    calls = {"n": 0}

    def rebinding_getaddrinfo(host, *args, **kwargs):
        if host == "rebind.example.test":
            calls["n"] += 1
            answer = "93.184.216.34" if calls["n"] == 1 else "127.0.0.1"
            return real_getaddrinfo(answer, *args, **kwargs)
        return real_getaddrinfo(host, *args, **kwargs)

    orig_getaddrinfo = net.socket.getaddrinfo
    net.socket.getaddrinfo = rebinding_getaddrinfo
    try:
        client, _ = net.http_client(5.0)
        with client, pytest.raises(ValueError, match="private"):
            client.get("http://rebind.example.test:9/")
    finally:
        net.socket.getaddrinfo = orig_getaddrinfo

    # The first call is the request hook's own check; every later call belongs
    # to the transport pinning the connection — and it is that later call's
    # answer that decided the outcome above, not the first one.
    assert calls["n"] >= 2


def test_the_pinning_transport_connects_to_the_address_it_resolved_not_a_hostname(
    monkeypatch,
):
    """Direct proof of the mechanism: the underlying connection never receives
    the hostname at all (so it has nothing left to re-resolve), while Host and
    SNI still carry the real name for routing and certificate verification."""
    import httpx

    from seohead.recon import net

    monkeypatch.setattr(
        net.socket,
        "getaddrinfo",
        lambda host, port, *a, **k: [
            (net.socket.AF_INET, net.socket.SOCK_STREAM, 6, "", ("93.184.216.34", port))
        ],
    )
    monkeypatch.delenv(net.PRIVATE_NETWORK_ENV, raising=False)

    captured = {}

    def fake_connect(self, request):
        captured["host"] = request.url.host
        captured["sni_hostname"] = request.extensions.get("sni_hostname")
        captured["host_header"] = request.headers.get("host")
        return httpx.Response(200, content=b"ok", request=request)

    monkeypatch.setattr(httpx.HTTPTransport, "handle_request", fake_connect)

    client, _ = net.http_client(5.0)
    with client:
        response = client.get("https://rebind.example.test/")

    assert response.status_code == 200
    assert captured["host"] == "93.184.216.34"
    assert captured["sni_hostname"] == "rebind.example.test"
    assert captured["host_header"] == "rebind.example.test"


def test_a_redirect_to_a_private_address_is_refused_by_the_transport_alone(monkeypatch):
    """Issue #142's "one more step" variant: a 302 pointing at a private
    target. Built with no event hooks at all, so a pass here can only be the
    transport itself re-validating and re-pinning the second hop — not the
    request/response hooks that already covered this at the httpx-hook layer
    in test_public_safety.py."""
    import httpx

    from seohead.recon import net

    def fake_getaddrinfo(host, port, *_args, **_kwargs):
        address = "10.0.0.5" if host == "internal.example.test" else "93.184.216.34"
        return [(net.socket.AF_INET, net.socket.SOCK_STREAM, 6, "", (address, port))]

    monkeypatch.setattr(net.socket, "getaddrinfo", fake_getaddrinfo)
    monkeypatch.delenv(net.PRIVATE_NETWORK_ENV, raising=False)

    def fake_connect(self, request):
        if request.url.host == "93.184.216.34":
            return httpx.Response(
                302,
                headers={"Location": "http://internal.example.test/secret"},
                content=b"",
                request=request,
            )
        raise AssertionError("must never connect for the private redirect target")

    monkeypatch.setattr(httpx.HTTPTransport, "handle_request", fake_connect)

    transport = net._get_pinning_transport_cls()()
    client = httpx.Client(transport=transport, follow_redirects=True)
    with pytest.raises(ValueError, match="private"):
        client.get("https://start.example.test/")


# ── circuit breaker ─────────────────────────────────────────────────────────


def test_a_single_429_is_treated_as_an_overload_signal():
    from seohead.crawl.throttle import Throttle

    t = Throttle(min_delay=0.5)
    before = t.delay
    t.record_server_error(429)
    assert t.delay > before * 2


def test_retry_after_raises_the_delay_to_at_least_what_was_asked():
    from seohead.crawl.throttle import Throttle

    t = Throttle(min_delay=0.5)
    t.record_server_error(503, retry_after=30.0)
    assert t.delay >= 30.0


def test_a_non_numeric_retry_after_is_not_mistaken_for_a_duration():
    from seohead.crawl.collect import _retry_after

    assert _retry_after("Wed, 21 Oct 2026 07:28:00 GMT") is None
    assert _retry_after("120") == 120.0
    assert _retry_after(None) is None


def test_repeated_server_refusals_stop_the_crawl():
    site = {
        "https://example.com/robots.txt": FakeResponse("User-agent: *\n", ct="text/plain"),
        "https://example.com/": page(*[f"/p{i}" for i in range(9)]),
    }
    for i in range(9):
        site[f"https://example.com/p{i}"] = FakeResponse("", status_code=503)
    result = crawl_site(
        "https://example.com/",
        min_delay=0,
        sleeper=lambda _s: None,
        fetcher=lambda u: site.get(u) or FakeResponse("", 404),
        max_urls=50,
    )
    assert result.partial is True
    assert "refused repeatedly" in result.stopped_reason


def test_a_success_clears_the_refusal_streak():
    from seohead.crawl.throttle import Throttle

    t = Throttle()
    for _ in range(4):
        t.record_server_error(503)
    t.record_success()
    assert t.host_is_failing() is False


def test_a_connection_failure_trips_the_breaker_like_a_timeout():
    """#132: a ConnectionResetError's message never contains the word "timeout",
    so a breaker keyed on that substring stayed at zero no matter how many times
    a dead host refused the connection. Classification is by exception type now
    (see _classify_fetch_error), so a connection failure must feed the same
    counter a real timeout does."""
    from seohead.crawl.throttle import Throttle

    def fetcher(_url):
        raise ConnectionResetError("Connection reset by peer")

    throttle = Throttle()
    for _ in range(5):
        record, parsed = fetch_one("https://example.com/", fetcher=fetcher, throttle=throttle)
        assert parsed is None
        assert record.error_kind == "connection"
    assert throttle.timeouts == 5
    assert throttle.should_stop(limit=5) is True


# ── operator concurrency policy ─────────────────────────────────────────────


def test_throttle_keeps_the_operator_configured_concurrency_limit():
    """Adaptive control starts conservatively, but does not replace policy."""
    from seohead.crawl.throttle import Throttle

    t = Throttle(max_concurrency=999)
    assert t.max_concurrency == 999


def test_crawl_keeps_the_operator_configured_concurrency_limit():
    site = {
        "https://example.com/robots.txt": FakeResponse("User-agent: *\n", ct="text/plain"),
        "https://example.com/": page(*[f"/p{i}" for i in range(30)]),
        **{f"https://example.com/p{i}": page() for i in range(30)},
    }
    result = crawl_site(
        "https://example.com/",
        min_delay=0,
        sleeper=lambda _s: None,
        fetcher=lambda u: site.get(u) or FakeResponse("", 404),
        max_urls=50,
        concurrency=999,
    )
    assert result.effective_concurrency <= 999


# ── credential headers ──────────────────────────────────────────────────────


def test_fetch_one_sends_credential_headers_through_the_real_client(monkeypatch):
    """extra_headers must reach the request, not just be accepted and dropped."""
    import seohead.crawl.collect as collect_mod

    monkeypatch.setattr(collect_mod, "validate_url", lambda u: u)
    monkeypatch.setattr(
        collect_mod,
        "pinned_target",
        lambda u: (u, {"Host": "example.com"}, {"sni_hostname": "example.com"}),
    )
    captured = {}

    class FakeClient:
        def get(self, target, *, headers, extensions):
            captured["headers"] = headers
            return FakeResponse("<html><head><title>t</title></head><body></body></html>")

    record, _ = fetch_one(
        "https://example.com/",
        client=FakeClient(),
        extra_headers={"Authorization": "Bearer secret-token"},
    )
    assert captured["headers"]["Authorization"] == "Bearer secret-token"
    assert record.status_code == 200


def test_the_spider_resolves_credentials_per_hop_by_that_hops_own_host(monkeypatch):
    """A stale host from an earlier hop must never decide a later request's headers."""
    import seohead.crawl.spider as spider_mod

    seen_hosts = []
    monkeypatch.setattr(
        spider_mod,
        "resolve_credential_headers",
        lambda entries, host: seen_hosts.append(host) or {},
    )
    site = {
        "https://example.com/robots.txt": FakeResponse("User-agent: *\n", ct="text/plain"),
        "https://example.com/": page("/a"),
        "https://example.com/a": page(),
    }
    crawl_site(
        "https://example.com/",
        min_delay=0,
        sleeper=lambda _s: None,
        fetcher=lambda u: site.get(u) or FakeResponse("", 404),
        credential_headers=[{"host": "example.com", "headers": {}}],
    )
    assert seen_hosts == ["example.com", "example.com"]


def test_list_mode_never_resolves_one_hosts_credentials_for_another(monkeypatch):
    """The direct shape of "dropped on cross-host redirect": a list crawl can name
    URLs on several hosts, and a credential bound to one must not follow to another.
    """
    import seohead.crawl.collect as collect_mod

    seen_hosts = []
    monkeypatch.setattr(
        collect_mod,
        "resolve_credential_headers",
        lambda entries, host: seen_hosts.append(host) or {},
    )
    collect_urls(
        ["https://a.example.com/", "https://b.example.com/"],
        min_delay=0,
        sleeper=lambda _s: None,
        fetcher=lambda _u: page(),
        credential_headers=[{"host": "a.example.com", "headers": {}}],
    )
    assert seen_hosts == ["a.example.com", "b.example.com"]

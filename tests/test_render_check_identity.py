"""render_check() against a stub Playwright and a stub HTTP client, never a real browser or
the network -- see test_render_document.py for the same discipline applied to
render_document().

#199: the raw fetch and the rendered fetch must present the same request identity to the
origin. Without that, Chromium's own default User-Agent reaches the origin while the raw
fetch used the toolkit's identifiable one, and a server that varies its response by
User-Agent -- legal, common, and unrelated to JavaScript -- becomes indistinguishable from a
page that genuinely needs a renderer.
"""

from __future__ import annotations

import sys
import types

import httpx
import pytest

from seohead import cli
from seohead.checks import render as render_module
from seohead.checks.render import render_check
from seohead.mcp import handlers
from seohead.recon.net import UA


class _FakeResponse:
    def __init__(self, text, url="https://example.com/", status_code=200):
        self.text = text
        self.url = url
        self.status_code = status_code


class _FakeHttpClient:
    def __init__(self, response):
        self._response = response
        self.closed = False

    def get(self, _url):
        return self._response

    def close(self):
        self.closed = True


class _FakePage:
    def __init__(self, html, goto_error=None):
        self.html = html
        self.url = "https://example.com/"
        self.routes = []
        self.goto_error = goto_error
        self.goto_calls = []
        self.load_states = []
        self.settled_ms = []

    def on(self, event, handler):
        pass

    def route(self, pattern, handler):
        self.routes.append((pattern, handler))

    def goto(self, _url, wait_until=None, timeout=None):
        self.goto_calls.append(wait_until)
        if self.goto_error is not None:
            raise self.goto_error

    def wait_for_load_state(self, state, timeout=None):
        self.load_states.append(state)

    def wait_for_timeout(self, milliseconds):
        self.settled_ms.append(milliseconds)

    def content(self):
        return self.html

    def evaluate(self, script):
        # Stands in for both _METRICS_JS and _BACKGROUND_IMAGES_JS -- render_check only
        # needs a shape it can iterate/index, not real Core Web Vitals.
        return [] if "backgroundImage" in script else {}


class _FakeContext:
    def __init__(self, page):
        self.page = page
        self.options: dict[str, object] = {}
        self.routes = []
        self.new_page_route_snapshots = []

    def add_init_script(self, _script):
        pass

    def route_web_socket(self, _pattern, _handler):
        pass

    def route(self, pattern, handler):
        self.routes.append((pattern, handler))

    def new_page(self):
        self.new_page_route_snapshots.append(list(self.routes))
        return self.page

    def close(self):
        pass


class _FakeBrowser:
    def __init__(self, context):
        self._context = context

    def new_context(self, **options):
        self._context.options.update(options)
        return self._context

    def close(self):
        pass


class _FakeChromium:
    def __init__(self, browser):
        self._browser = browser
        self.launch_calls = []

    def launch(self, **options):
        self.launch_calls.append(options)
        return self._browser


class _FakePlaywright:
    def __init__(self, chromium):
        self.chromium = chromium

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


def _install_stack(monkeypatch, raw_html, rendered_html, *, goto_error=None, timeout_error=None):
    """Point ``render_module`` at a fake browser and a fake origin, and hand back the parts.

    ``timeout_error`` is the class the fake ``playwright.sync_api`` exposes as
    ``TimeoutError``; leaving it unset is the harness the identity tests already
    used, and also covers a module that exposes only ``sync_playwright``.
    """
    page = _FakePage(rendered_html, goto_error=goto_error)
    context = _FakeContext(page)
    browser = _FakeBrowser(context)
    chromium = _FakeChromium(browser)
    pw = _FakePlaywright(chromium)

    fake_playwright = types.ModuleType("playwright")
    fake_sync_api = types.ModuleType("playwright.sync_api")
    fake_sync_api.sync_playwright = lambda: pw
    if timeout_error is not None:
        fake_sync_api.TimeoutError = timeout_error
    fake_playwright.sync_api = fake_sync_api
    monkeypatch.setitem(sys.modules, "playwright", fake_playwright)
    monkeypatch.setitem(sys.modules, "playwright.sync_api", fake_sync_api)

    http_calls = []

    def fake_http_client(_timeout, **kwargs):
        http_calls.append(kwargs)
        return _FakeHttpClient(_FakeResponse(raw_html)), True

    monkeypatch.setattr(render_module, "http_client", fake_http_client)
    monkeypatch.setattr(render_module, "validate_url", lambda url: url)
    monkeypatch.setattr(render_module, "_refuse_if_root", lambda: None)

    return {
        "page": page,
        "context": context,
        "browser": browser,
        "chromium": chromium,
        "http_calls": http_calls,
    }


@pytest.fixture
def fake_stack(monkeypatch):
    monkeypatch.delenv("SEOHEAD_CHROME", raising=False)
    """A raw fetch and a rendered fetch of two script-free documents -- shaped after #199's
    own offline reproducer, so the harness stands for exactly the case it describes: a
    server-side User-Agent variant with no JavaScript involved anywhere.
    """
    raw_html = (
        "<html><head><title>Raw</title></head><body><p>" + "raw " * 100 + "</p></body></html>"
    )
    rendered_html = (
        "<html><head><title>Raw</title></head><body><p>"
        + "raw " * 100
        + "chromium " * 220
        + "</p></body></html>"
    )
    return _install_stack(monkeypatch, raw_html, rendered_html)


def test_the_rendered_browser_context_shares_the_raw_fetchs_user_agent(fake_stack):
    render_check("https://example.com/")
    assert fake_stack["context"].options.get("user_agent") == UA


def test_the_shared_identity_is_recorded_in_the_result(fake_stack):
    result = render_check("https://example.com/")
    assert result["ok"] is True
    assert result["user_agent"] == UA


def test_raw_and_browser_clients_share_the_crawl_proxy_route(fake_stack):
    from seohead.recon.net import ProxyRoute

    route = ProxyRoute(
        proxy=object(), identity="http://proxy.example.test:3128", authenticated=False
    )
    result = render_check("https://example.com/", proxy_route=route)

    assert result["ok"] is True
    assert len(fake_stack["http_calls"]) == 2
    assert all(call["proxy_route"] is route for call in fake_stack["http_calls"])
    assert all(call["trust_env"] is False for call in fake_stack["http_calls"])


def test_mobile_render_check_uses_one_mobile_identity_for_raw_and_browser(fake_stack):
    """#670: a mobile viewport must reach the mobile dynamic-serving branch."""
    result = render_check("https://example.com/", viewport="mobile")

    assert result["ok"] is True
    assert result["viewport"] == "mobile"
    assert "Mobile" in result["user_agent"]
    assert result["viewport_size"] == {"width": 390, "height": 844}
    assert fake_stack["context"].options["user_agent"] == result["user_agent"]
    assert all(
        call["headers"]["User-Agent"] == result["user_agent"] for call in fake_stack["http_calls"]
    )


def test_explicit_render_identity_overrides_both_mobile_requests(fake_stack):
    """A deliberate diagnostic UA must not split raw and browser representations."""
    custom = "ExampleMobileAudit/1.0"
    result = render_check("https://example.com/", viewport="mobile", user_agent=custom)

    assert result["user_agent"] == custom
    assert fake_stack["context"].options["user_agent"] == custom
    assert all(call["headers"]["User-Agent"] == custom for call in fake_stack["http_calls"])


def test_desktop_identity_stays_the_toolkit_default(fake_stack):
    result = render_check("https://example.com/", viewport="desktop")

    assert result["user_agent"] == UA
    assert result["viewport_size"] == {"width": 1366, "height": 768}


def test_invalid_user_agent_is_refused_before_a_request(fake_stack):
    result = render_check("https://example.com/", user_agent="bad\nheader")

    assert result == {"ok": False, "error": "user_agent must be a single header line"}
    assert fake_stack["http_calls"] == []


def test_handler_and_cli_forward_an_explicit_render_identity(monkeypatch):
    received = []
    monkeypatch.setattr(
        render_module,
        "render_check",
        lambda url, **kwargs: received.append({"url": url, **kwargs}) or {"ok": True},
    )

    assert handlers.render_check(
        url="https://example.com/", viewport="mobile", wait="load", user_agent="Example/1.0"
    ) == {"ok": True}
    assert received == [
        {
            "url": "https://example.com/",
            "viewport": "mobile",
            "wait": "load",
            "user_agent": "Example/1.0",
        }
    ]

    args = cli.build_parser().parse_args(
        [
            "render-check",
            "--url",
            "https://example.com/",
            "--viewport",
            "mobile",
            "--user-agent",
            "Example/1.0",
        ]
    )
    _name, kwargs = cli._build_kwargs("render-check", args)
    assert kwargs["user_agent"] == "Example/1.0"


def test_each_render_entry_registers_the_pinned_route_before_new_page(fake_stack):
    checked = render_check("https://example.com/")
    rendered = render_module.rendered_html("https://example.com/")

    assert checked["ok"] is True
    assert rendered["ok"] is True
    assert all(
        snapshot[-1][0] == "**/*" for snapshot in fake_stack["context"].new_page_route_snapshots
    )
    assert [pattern for pattern, _handler in fake_stack["context"].routes] == ["**/*", "**/*"]
    assert fake_stack["page"].routes == []


def test_render_check_and_rendered_html_require_the_chromium_sandbox(fake_stack):
    render_check("https://example.com/")
    render_module.rendered_html("https://example.com/")

    assert fake_stack["chromium"].launch_calls == [
        {"chromium_sandbox": True},
        {"chromium_sandbox": True},
    ]


def test_rendered_html_reports_a_sandbox_launch_failure(fake_stack, monkeypatch):
    attempts = []

    def fail_to_launch(**options):
        attempts.append(options)
        assert options == {"chromium_sandbox": True}
        raise RuntimeError("sandbox launch unavailable")

    monkeypatch.setattr(fake_stack["chromium"], "launch", fail_to_launch)

    result = render_module.rendered_html("https://example.com/")

    assert result["ok"] is False
    assert "sandbox launch unavailable" in result["error"]
    assert attempts == [{"chromium_sandbox": True}]


class _PinnedRequest:
    url = "https://example.com/"
    method = "GET"

    def all_headers(self):
        return {"accept": "text/html", "host": "example.com"}


class _PinnedRoute:
    request = _PinnedRequest()

    def __init__(self):
        self.fulfilled = []
        self.aborted = []

    def fulfill(self, **kwargs):
        self.fulfilled.append(kwargs)

    def abort(self, reason):
        self.aborted.append(reason)


class _PinnedStream(httpx.SyncByteStream):
    def __init__(self, body):
        self.body = body

    def __iter__(self):
        yield self.body

    def close(self):
        pass


def _deliver_route_during_navigation(monkeypatch, fake_stack, route):
    original_goto = fake_stack["page"].goto

    def goto(url, **kwargs):
        original_goto(url, **kwargs)
        fake_stack["context"].routes[-1][1](route)

    monkeypatch.setattr(fake_stack["page"], "goto", goto)


def test_rendered_html_fulfils_its_registered_pinned_route(monkeypatch, fake_stack):
    expected_html = "<html><body>pinned response</body></html>"

    client = httpx.Client(
        transport=httpx.MockTransport(
            lambda _request: httpx.Response(200, stream=_PinnedStream(expected_html.encode()))
        )
    )
    monkeypatch.setattr(render_module, "http_client", lambda *_args, **_kwargs: (client, False))
    fake_stack["page"].html = expected_html
    route = _PinnedRoute()
    gate_calls = []
    _deliver_route_during_navigation(monkeypatch, fake_stack, route)

    result = render_module.rendered_html(
        "https://example.com/", request_gate=lambda: gate_calls.append("called")
    )

    assert result == {
        "ok": True,
        "url": "https://example.com/",
        "html": expected_html,
        "representation": "rendered",
        "dom_bytes": len(expected_html.encode("utf-8")),
        "line_count": 1,
    }
    assert route.fulfilled[0]["body"] == expected_html.encode()
    assert route.aborted == []
    assert gate_calls == ["called"]
    assert client.is_closed


def test_render_check_gates_each_raw_redirect_and_pinned_browser_request(monkeypatch, fake_stack):
    clients = []
    raw_requests = []

    def raw_transport(request):
        raw_requests.append(str(request.url))
        if len(raw_requests) == 1:
            return httpx.Response(302, headers={"location": "/redirected"}, request=request)
        return httpx.Response(200, text=fake_stack["page"].html, request=request)

    def http_client(_timeout, **kwargs):
        if "event_hooks" in kwargs:
            client = httpx.Client(
                transport=httpx.MockTransport(raw_transport),
                follow_redirects=True,
                event_hooks=kwargs["event_hooks"],
            )
        else:
            client = httpx.Client(
                transport=httpx.MockTransport(
                    lambda _request: httpx.Response(
                        200, stream=_PinnedStream(b"<html><body>pinned response</body></html>")
                    )
                )
            )
        clients.append(client)
        return client, False

    monkeypatch.setattr(render_module, "http_client", http_client)
    route = _PinnedRoute()
    gate_calls = []
    _deliver_route_during_navigation(monkeypatch, fake_stack, route)

    result = render_check("https://example.com/", request_gate=lambda: gate_calls.append("called"))

    assert result["ok"] is True
    assert len(raw_requests) == 2
    assert len(gate_calls) == 3
    assert route.fulfilled[0]["body"] == b"<html><body>pinned response</body></html>"
    assert all(client.is_closed for client in clients)


# ── An unfinished render is unavailable, not a site defect (#623) ────────────


class _FakeTimeoutError(Exception):
    """Stands in for ``playwright.sync_api.TimeoutError``."""


_RAW_PAGE = (
    "<html><head><title>Профессия :: Profiz.ru</title>"
    '<link rel="canonical" href="https://example.com/"></head><body><h1>Rubric</h1>'
    + "<p>"
    + "text " * 400
    + "</p>"
    + "".join(f'<a href="/sr/rubric/{n}/">rubric {n}</a>' for n in range(40))
    + "</body></html>"
)

# What the browser hands back when the render never finishes: a document with
# none of the page in it, at a fraction of the raw response's size.
_TRUNCATED_RENDER = "<html><head></head><body><div></div></body></html>"


def test_an_unfinished_render_is_reported_unavailable_with_a_named_reason(monkeypatch):
    """#623: this exact pair produced 'the title changes after JavaScript' and
    'the canonical is injected by JavaScript' on a live site, four times over."""
    _install_stack(monkeypatch, _RAW_PAGE, _TRUNCATED_RENDER)

    result = render_check("https://example.com/sr/rubric/1/")

    assert result["ok"] is False
    assert result["reason"] == "incomplete_render"
    assert "comparison is unavailable" in result["error"]
    assert "findings" not in result
    # Not measured is not clean, and not a defect either.
    assert result["js_dependent"] is None
    # Both snapshots ride along so the reason can be checked, not just believed.
    assert result["raw"]["title"] == "Профессия :: Profiz.ru"
    assert result["rendered"]["title"] == ""


def test_a_completed_render_still_reports_findings_and_a_verdict(monkeypatch):
    """The guard must not turn every render into an unavailable measurement: a
    page whose title a script genuinely rewrote still fires its finding."""
    rendered = _RAW_PAGE.replace("Профессия :: Profiz.ru", "Rewritten by script")
    _install_stack(monkeypatch, _RAW_PAGE, rendered)

    result = render_check("https://example.com/sr/rubric/1/")

    assert result["ok"] is True
    assert "reason" not in result
    assert result["js_dependent"] is True
    assert any("title changes after JavaScript" in f for f in result["findings"])


def test_a_milestone_that_never_arrives_falls_back_to_domcontentloaded(monkeypatch):
    """A site with long-polling analytics or chat never goes network-idle. That
    is ordinary: read the DOM at domcontentloaded rather than lose the check."""
    stack = _install_stack(
        monkeypatch,
        _RAW_PAGE,
        _RAW_PAGE,
        goto_error=_FakeTimeoutError("Timeout 30000ms exceeded"),
        timeout_error=_FakeTimeoutError,
    )

    result = render_check("https://example.com/", wait="networkidle")

    assert result["ok"] is True
    assert result["wait"] == "networkidle"
    assert result["wait_reached"] == "domcontentloaded"
    assert stack["page"].load_states == ["domcontentloaded"]
    # One navigation, not two: the document is already there to be read.
    assert stack["page"].goto_calls == ["networkidle"]


def test_a_document_that_never_loaded_at_all_stays_a_rendering_failure(monkeypatch):
    """The fallback recovers a late page, not a page that never arrived."""

    def never_loaded(_state, timeout=None):
        raise _FakeTimeoutError("Timeout 30000ms exceeded")

    stack = _install_stack(
        monkeypatch,
        _RAW_PAGE,
        _RAW_PAGE,
        goto_error=_FakeTimeoutError("Timeout 30000ms exceeded"),
        timeout_error=_FakeTimeoutError,
    )
    monkeypatch.setattr(stack["page"], "wait_for_load_state", never_loaded)

    result = render_check("https://example.com/", wait="networkidle")

    assert result["ok"] is False
    assert "Browser rendering failed" in result["error"]
    assert result.get("reason") != "incomplete_render"


def test_the_dom_is_read_after_a_short_settle(monkeypatch):
    """Deferred scripts write the DOM after the milestone resolves."""
    stack = _install_stack(monkeypatch, _RAW_PAGE, _RAW_PAGE)

    result = render_check("https://example.com/")

    assert result["settle_ms"] == render_module.SETTLE_MS
    assert stack["page"].settled_ms == [render_module.SETTLE_MS]


def test_the_settle_can_be_switched_off(monkeypatch):
    stack = _install_stack(monkeypatch, _RAW_PAGE, _RAW_PAGE)

    render_check("https://example.com/", settle_ms=0)

    assert stack["page"].settled_ms == []


# ── An unfinished render must not read as a clean one (#642) ─────────────────


# A single-page-application shell: the raw response carries a title, a canonical
# and an empty mount point, and no page copy at all. Exactly the page whose
# render is most likely to time out -- which is why the shell finding must not
# be discarded together with the render.
_SPA_SHELL_PAGE = (
    '<html><head><title>Shop</title><link rel="canonical" href="https://example.com/">'
    '</head><body><div id="root"></div><a href="/catalogue/">Catalogue</a>'
    "<!--" + "x" * 5000 + "--></body></html>"
)

# The same page without the mount point: a server-rendered document, so a failed
# render here is a failed render and nothing more.
_SERVER_RENDERED_PAGE = _RAW_PAGE


def test_a_missed_milestone_is_stated_in_the_findings_and_withholds_the_all_clear(monkeypatch):
    """#642: the fallback capture read the DOM before the scripts ran, so raw and
    rendered were identical and the run asserted that JavaScript does not matter
    on a page nobody rendered. seohead.audit.site carries findings text only, so
    the miss has to be in that list."""
    _install_stack(
        monkeypatch,
        _RAW_PAGE,
        _RAW_PAGE,
        goto_error=_FakeTimeoutError("Timeout 30000ms exceeded"),
        timeout_error=_FakeTimeoutError,
    )

    result = render_check("https://example.com/")

    assert result["wait"] == "load"
    assert result["wait_reached"] == "domcontentloaded"
    assert render_module.ALL_CLEAR not in result["findings"]
    assert any("milestone was never reached" in f for f in result["findings"])
    assert any("domcontentloaded" in f for f in result["findings"])
    # Not measured is not clean: None, never False.
    assert result["js_dependent"] is None


def test_a_render_that_reached_its_milestone_still_reports_the_all_clear(monkeypatch):
    """The other direction: the guard must not withhold a verdict from a run that
    did what it was asked and found nothing."""
    stack = _install_stack(monkeypatch, _RAW_PAGE, _RAW_PAGE)

    result = render_check("https://example.com/")

    assert stack["page"].load_states == []
    assert result["wait_reached"] == result["wait"] == "load"
    assert result["findings"] == [render_module.ALL_CLEAR]
    assert result["js_dependent"] is False


def test_an_empty_shell_survives_a_render_that_never_finished(monkeypatch):
    """#642: detect_empty_shell() reads the raw response and never the browser,
    so its answer holds whether or not the render finished."""
    _install_stack(monkeypatch, _SPA_SHELL_PAGE, _TRUNCATED_RENDER)

    result = render_check("https://example.com/")

    assert result["ok"] is False
    assert result["reason"] == "incomplete_render"
    # The key is carried, not omitted, so a caller can read the raw-HTML answer
    # out of an incomplete result.
    assert result["empty_shell"] == "root"
    assert result["js_dependent"] is None
    # compare() keeps the raw-derived finding alongside the unavailability
    # statement, for the callers that read findings rather than the key.
    findings = render_module.compare(
        result["raw"], result["rendered"], _SPA_SHELL_PAGE, result["empty_shell"]
    )
    assert any("comparison is unavailable" in f for f in findings)
    assert any('empty <div id="root">' in f for f in findings)


def test_a_failed_render_of_a_server_rendered_page_reports_no_shell(monkeypatch):
    """The other direction: the shell finding is evidence, not consolation. A page
    with no empty mount point must not acquire one because its render failed."""
    _install_stack(monkeypatch, _SERVER_RENDERED_PAGE, _TRUNCATED_RENDER)

    result = render_check("https://example.com/")

    assert result["ok"] is False
    assert result["reason"] == "incomplete_render"
    assert result["empty_shell"] is None
    findings = render_module.compare(
        result["raw"], result["rendered"], _SERVER_RENDERED_PAGE, result["empty_shell"]
    )
    assert findings == [result["error"]]


@pytest.mark.parametrize("identity", [False, 0, "\x00", "\t", "   ", "Agent-\u2603"])
def test_invalid_render_identity_is_refused_before_http(monkeypatch, identity):
    calls = []

    def unexpected_http(*args, **kwargs):
        calls.append((args, kwargs))
        raise AssertionError("invalid identity reached HTTP setup")

    monkeypatch.setattr(render_module, "http_client", unexpected_http)
    result = render_module.render_check("https://example.test/", user_agent=identity)
    assert result["ok"] is False
    assert "user_agent" in result["error"]
    assert calls == []

"""render_document() against a stub Playwright, never a real browser or the
network (#18): the stub records every call so the wiring — which settings
reach which Playwright call — is what gets asserted, not Chromium's actual
behaviour.
"""

from __future__ import annotations

import os
import socket
import sys
import types

import pytest

from seohead.checks import render
from seohead.checks.render import render_document
from seohead.crawl import settings as crawl_config
from seohead.crawl import sqlite_render


def _fixture_dns(host, port, *args, **kwargs):
    """Resolve only synthetic public test hosts while retaining URL policy checks."""
    if host == "127.0.0.1":
        address = host
    elif host in {"example.com", "example.test"}:
        address = "93.184.216.34"
    else:
        raise socket.gaierror("fixture has no DNS record for this host")
    return [(socket.AF_INET, socket.SOCK_STREAM, socket.IPPROTO_TCP, "", (address, port))]


class _ConsoleMsg:
    def __init__(self, type_, text):
        self.type = type_
        self.text = text


class _FakePage:
    def __init__(self, html="<html><body>rendered</body></html>", scroll_height=500):
        self.html = html
        self.url = "https://example.com/"
        self.scroll_height = scroll_height
        self.routes = []
        self.handlers = {}
        self.evaluated = []
        self.wait_calls = []
        self.viewport_calls = []
        self.screenshot_calls = []
        self.goto_calls = []
        # Console messages "emitted" during this fake page's navigation, fed
        # to whatever handler render_document() registered via page.on().
        self.console_messages: list[_ConsoleMsg] = []

    def route(self, pattern, handler):
        self.routes.append((pattern, handler))

    def on(self, event, handler):
        self.handlers[event] = handler

    def goto(self, url, wait_until=None, timeout=None):
        self.goto_calls.append({"url": url, "wait_until": wait_until, "timeout": timeout})
        console_handler = self.handlers.get("console")
        if console_handler:
            for msg in self.console_messages:
                console_handler(msg)

    def wait_for_timeout(self, ms):
        self.wait_calls.append(ms)

    def evaluate(self, script):
        self.evaluated.append(script)
        if "scrollHeight" in script:
            return self.scroll_height
        return None

    def set_viewport_size(self, size):
        self.viewport_calls.append(size)

    def content(self):
        return self.html

    def screenshot(self, path=None, full_page=None):
        self.screenshot_calls.append({"path": path, "full_page": full_page})


class _FakeContext:
    def __init__(self, page, **options):
        self.page = page
        self.options = options
        self.routes = []
        self.new_page_route_snapshots = []
        self.ws_routes = []
        self.closed = False

    def new_page(self):
        self.new_page_route_snapshots.append(list(self.routes))
        return self.page

    def route(self, pattern, handler):
        self.routes.append((pattern, handler))

    def route_web_socket(self, pattern, handler):
        self.ws_routes.append((pattern, handler))

    def close(self):
        self.closed = True


class _FakeBrowser:
    def __init__(self, context):
        self._context = context
        self.closed = False

    def new_context(self, **options):
        self._context.options.update(options)
        return self._context

    def close(self):
        self.closed = True


class _FakeChromium:
    def __init__(self, browser, context):
        self._browser = browser
        self._context = context
        self.launched = False
        self.launch_calls = []
        self.launch_persistent_calls = []

    def launch(self, **options):
        self.launched = True
        self.launch_calls.append(options)
        return self._browser

    def launch_persistent_context(self, user_data_dir, **options):
        self.launch_persistent_calls.append((user_data_dir, options))
        self._context.options.update(options)
        return self._context


class _FakePlaywright:
    def __init__(self, chromium):
        self.chromium = chromium

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class _FakeLauncher:
    """One browser-type launcher; ``fail`` stands in for a launch-time error."""

    def __init__(self, browser, fail=None):
        self._browser = browser
        self.fail = fail
        self.launched = False
        self.launch_calls = []

    def launch(self, **options):
        self.launched = True
        self.launch_calls.append(options)
        if self.fail is not None:
            raise self.fail
        return self._browser


class _FakeEnginePlaywright:
    """A Playwright stand-in exposing one launcher per supported engine."""

    def __init__(self, launchers):
        for name, launcher in launchers.items():
            setattr(self, name, launcher)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


@pytest.fixture
def fake_stack(monkeypatch):
    """Stand in for the real ``playwright`` package via ``sys.modules``.

    render_document() does ``from playwright.sync_api import sync_playwright``
    at call time, so it never needs the real package to be installed --
    which is the point: rendering tests must pass in the plain ``pytest``
    matrix, where the optional ``render`` extra is not installed at all.
    ``monkeypatch.setattr("playwright.sync_api.sync_playwright", ...)`` would
    still import the real module just to resolve that dotted path, so a
    fake module registered directly in ``sys.modules`` is what actually
    keeps this test file independent of whether playwright is installed.
    """
    monkeypatch.delenv("SEOHEAD_CHROME", raising=False)
    # Resolve synthetic fixture names to a public IP; keep validate_url's actual
    # safety policy active without DNS or network access.
    monkeypatch.setattr(socket, "getaddrinfo", _fixture_dns)
    page = _FakePage()
    context = _FakeContext(page)
    browser = _FakeBrowser(context)
    chromium = _FakeChromium(browser, context)
    pw = _FakePlaywright(chromium)

    fake_playwright = types.ModuleType("playwright")
    fake_sync_api = types.ModuleType("playwright.sync_api")
    fake_sync_api.sync_playwright = lambda: pw
    fake_playwright.sync_api = fake_sync_api
    monkeypatch.setitem(sys.modules, "playwright", fake_playwright)
    monkeypatch.setitem(sys.modules, "playwright.sync_api", fake_sync_api)

    return {"page": page, "context": context, "browser": browser, "chromium": chromium}


@pytest.fixture
def fake_engines(monkeypatch):
    """The same stub ``playwright``, but with a launcher for every engine.

    Each supported engine name must reach its own Playwright browser type --
    selecting ``firefox`` and watching ``chromium`` launch would be exactly
    the silent fallback #744 forbids, so the fixture records launches per
    engine rather than on one shared attribute.
    """
    monkeypatch.delenv("SEOHEAD_CHROME", raising=False)
    monkeypatch.setattr(socket, "getaddrinfo", _fixture_dns)
    page = _FakePage()
    context = _FakeContext(page)
    browser = _FakeBrowser(context)
    launchers = {name: _FakeLauncher(browser) for name in ("chromium", "firefox", "webkit")}
    pw = _FakeEnginePlaywright(launchers)

    fake_playwright = types.ModuleType("playwright")
    fake_sync_api = types.ModuleType("playwright.sync_api")
    fake_sync_api.sync_playwright = lambda: pw
    fake_playwright.sync_api = fake_sync_api
    monkeypatch.setitem(sys.modules, "playwright", fake_playwright)
    monkeypatch.setitem(sys.modules, "playwright.sync_api", fake_sync_api)

    return {
        "page": page,
        "context": context,
        "browser": browser,
        "launchers": launchers,
    }


def _rendering_config(**browser_overrides):
    resolved = crawl_config.load(overrides={"rendering.mode": "js"})
    resolved["rendering"]["browser"].update(browser_overrides)
    return resolved["rendering"]


def test_rendered_credential_policy_uses_the_target_host(monkeypatch):
    monkeypatch.setenv("RENDER_POLICY_TOKEN", "synthetic-test-token")
    settings = crawl_config.load(
        overrides={
            "http.credentials_acknowledged": True,
            "http.credential_headers": [
                {
                    "host": "private.example.test",
                    "headers": {"authorization": "env:RENDER_POLICY_TOKEN"},
                }
            ],
        }
    )

    assert sqlite_render._policy_facts(settings, "https://public.example.test/") == {
        "credentials_used": False,
        "cache_control_no_store": False,
    }
    assert sqlite_render._policy_facts(settings, "https://private.example.test/") == {
        "credentials_used": True,
        "cache_control_no_store": False,
    }


def test_happy_path_returns_the_rendered_html(fake_stack):
    result = render_document("https://example.com/", _rendering_config())
    assert result["ok"] is True
    assert result["html"] == "<html><body>rendered</body></html>"
    assert result["final_url"] == "https://example.com/"


def test_crawler_render_document_uses_the_same_pinned_proxy_route(fake_stack, monkeypatch):
    from seohead.checks import render
    from seohead.recon.net import ProxyRoute

    route = ProxyRoute(
        proxy=object(), identity="http://proxy.example.test:3128", authenticated=False
    )
    calls = []

    class Client:
        def close(self):
            pass

    monkeypatch.setattr(render, "validate_url", lambda url: url)
    monkeypatch.setattr(render, "_refuse_if_root", lambda: None)
    monkeypatch.setattr(
        render,
        "http_client",
        lambda timeout, **kwargs: (calls.append(kwargs) or Client(), True),
    )
    result = render_document("https://example.test/", _rendering_config(), proxy_route=route)

    assert result["ok"] is True
    assert len(calls) == 1
    assert calls[0]["proxy_route"] is route
    assert calls[0]["trust_env"] is False


def test_a_cookie_the_browser_carries_is_not_the_operators_credential(fake_stack, monkeypatch):
    """#656: a browser carries back whatever the site's own Set-Cookie gave it.

    This used to assert the opposite, because a request hook read the wire headers
    and upgraded ``credentials_used`` on any ``Cookie:`` -- so one session cookie
    and a single same-origin subresource were enough to store the page's serialized
    DOM as ``credentialed`` on a run configured with no credentials at all. What
    the run was configured to send arrives as ``policy_facts``; the wire adds
    nothing to it.
    """

    class Request:
        def __init__(self):
            self.headers = {"accept": "text/html"}

        def all_headers(self):
            return {"accept": "text/html", "cookie": "session=redacted"}

    request = Request()
    original_goto = fake_stack["page"].goto
    original_evaluate = fake_stack["page"].evaluate

    def goto(url, **kwargs):
        original_goto(url, **kwargs)
        handler = fake_stack["page"].handlers.get("request")
        if handler is not None:
            handler(request)

    def evaluate(script):
        if "TextEncoder" in script:
            return {"complete": True, "bytes": 1, "html": fake_stack["page"].html}
        return original_evaluate(script)

    monkeypatch.setattr(fake_stack["page"], "goto", goto)
    monkeypatch.setattr(fake_stack["page"], "evaluate", evaluate)

    result = render_document("https://example.com/", _rendering_config(), max_html_bytes=1024)

    assert result["ok"] is True
    assert result["renderer"]["policy"] == {
        "credentials_used": False,
        "cache_control_no_store": False,
    }


def test_a_configured_credential_still_marks_the_dom_credentialed(fake_stack, monkeypatch):
    """The other direction, which must not weaken: what the run was configured to send."""
    original_evaluate = fake_stack["page"].evaluate

    def evaluate(script):
        if "TextEncoder" in script:
            return {"complete": True, "bytes": 1, "html": fake_stack["page"].html}
        return original_evaluate(script)

    monkeypatch.setattr(fake_stack["page"], "evaluate", evaluate)

    result = render_document(
        "https://example.com/",
        _rendering_config(),
        max_html_bytes=1024,
        policy_facts={"credentials_used": True, "cache_control_no_store": False},
    )

    assert result["ok"] is True
    assert result["renderer"]["policy"]["credentials_used"] is True


def test_pinned_route_is_registered_on_context_before_its_new_page(fake_stack):
    result = render_document("https://example.com/", _rendering_config())

    assert result["ok"] is True
    assert fake_stack["context"].routes[0][0] == "**/*"
    assert fake_stack["context"].new_page_route_snapshots == [fake_stack["context"].routes]
    assert fake_stack["page"].routes == []


def test_service_workers_are_blocked_by_default(fake_stack):
    render_document("https://example.com/", _rendering_config())
    assert fake_stack["context"].options["service_workers"] == "block"


def test_a_blocked_websocket_marks_the_rendered_dom_unavailable(fake_stack, monkeypatch):
    class WebSocket:
        def __init__(self):
            self.closed = False

        def close(self):
            self.closed = True

    websocket = WebSocket()
    original_goto = fake_stack["page"].goto

    def goto(url, **kwargs):
        original_goto(url, **kwargs)
        _pattern, handler = fake_stack["context"].ws_routes[0]
        handler(websocket)

    monkeypatch.setattr(fake_stack["page"], "goto", goto)

    result = render_document("https://example.com/", _rendering_config())

    assert result["ok"] is False
    assert "WebSocket" in result["error"]
    assert websocket.closed


def test_navigation_honours_the_configured_wait_until(fake_stack):
    render_document("https://example.com/", _rendering_config(wait_until="networkidle"))
    assert fake_stack["page"].goto_calls[0]["wait_until"] == "networkidle"


def test_navigation_events_distinguish_initial_spa_and_script_routes(fake_stack):
    page = fake_stack["page"]

    class Frame:
        url = ""

    frame = Frame()
    page.main_frame = frame

    def goto(url, **_kwargs):
        handler = page.handlers["framenavigated"]
        for destination in (url, url + "#pricing"):
            frame.url = destination
            handler(frame)
        request = type(
            "Request",
            (),
            {
                "url": "https://example.com/checkout",
                "frame": frame,
                "is_navigation_request": lambda self: True,
            },
        )()
        # A request alone cannot prove the navigation cause.
        assert request.url.endswith("checkout")
        frame.url = "https://example.com/checkout"
        handler(frame)

    page.goto = goto
    result = render_document("https://example.com/", _rendering_config())
    events = result["renderer"]["navigation"]["events"]
    assert [event["kind"] for event in events] == [
        "initial_http_navigation",
        "document_navigation",
        "document_navigation",
    ]
    assert all(event["user_click"] is None for event in events)


def test_script_timeout_is_a_wait_after_navigation(fake_stack):
    render_document("https://example.com/", _rendering_config(script_timeout_seconds=5))
    assert fake_stack["page"].wait_calls == [5000]


def test_zero_script_timeout_waits_not_at_all(fake_stack):
    render_document("https://example.com/", _rendering_config(script_timeout_seconds=0))
    assert fake_stack["page"].wait_calls == []


def test_resize_to_content_is_capped(fake_stack):
    fake_stack["page"].scroll_height = 99999
    render_document(
        "https://example.com/",
        _rendering_config(resize_to_content=True, resize_to_content_max_height_px=2000),
    )
    assert fake_stack["page"].viewport_calls[-1]["height"] == 2000


def test_resize_to_content_off_never_resizes(fake_stack):
    render_document("https://example.com/", _rendering_config(resize_to_content=False))
    assert fake_stack["page"].viewport_calls == []


# The dotted DEFAULTS paths behind the four tests below, spelled out so the coverage canary in
# test_crawl_settings_wired.py can see that they are exercised behaviourally here:
#   rendering.browser.flatten_shadow_dom
#   rendering.browser.flatten_iframes
#   rendering.browser.mobile_emulation
#   rendering.browser.touch_emulation
def test_flatten_shadow_dom_evaluates_the_flatten_script(fake_stack):
    render_document("https://example.com/", _rendering_config(flatten_shadow_dom=True))
    assert any("shadowRoot" in call for call in fake_stack["page"].evaluated)


def test_flatten_iframes_evaluates_the_flatten_script(fake_stack):
    render_document("https://example.com/", _rendering_config(flatten_iframes=True))
    assert any("contentDocument" in call for call in fake_stack["page"].evaluated)


def test_neither_flatten_script_runs_by_default(fake_stack):
    render_document("https://example.com/", _rendering_config())
    assert not any("shadowRoot" in call for call in fake_stack["page"].evaluated)
    assert not any("contentDocument" in call for call in fake_stack["page"].evaluated)


def test_device_emulation_settings_reach_the_context(fake_stack):
    render_document(
        "https://example.com/",
        _rendering_config(device_pixel_ratio=2.0, mobile_emulation=True, touch_emulation=True),
    )
    options = fake_stack["context"].options
    assert options["device_scale_factor"] == 2.0
    assert options["is_mobile"] is True
    assert options["has_touch"] is True


def test_console_errors_are_captured_when_enabled(fake_stack):
    config = _rendering_config()
    config["artifacts"]["console_errors"] = True
    fake_stack["page"].console_messages = [
        _ConsoleMsg("error", "boom"),
        _ConsoleMsg("log", "not an error"),
    ]
    result = render_document("https://example.com/", config)
    assert result["console_errors"] == ["boom"]


def test_console_messages_are_ignored_when_disabled(fake_stack):
    config = _rendering_config()
    config["artifacts"]["console_errors"] = False
    fake_stack["page"].console_messages = [_ConsoleMsg("error", "boom")]
    result = render_document("https://example.com/", config)
    assert result["console_errors"] == []


def test_screenshot_is_saved_when_enabled(fake_stack, tmp_path):
    config = _rendering_config()
    config["artifacts"]["screenshots"] = True
    result = render_document("https://example.com/", config, artifacts_dir=str(tmp_path))
    assert result["screenshot_path"] is not None
    assert fake_stack["page"].screenshot_calls[0]["full_page"] is True


def test_first_screen_screenshot_is_configurable(fake_stack, tmp_path):
    config = crawl_config.load(
        overrides={
            "rendering.mode": "js",
            "rendering.artifacts.screenshots": True,
            "rendering.artifacts.screenshot_mode": "first_screen",
        }
    )["rendering"]
    result = render_document(
        "https://example.com/",
        config,
        artifacts_dir=str(tmp_path),
    )
    assert result["screenshot_state"] == "staged"
    assert fake_stack["page"].screenshot_calls[0]["full_page"] is False


def test_rendering_action_and_preset_settings_reach_page_actions(fake_stack, monkeypatch):
    from seohead.checks import page_actions

    observed = {}

    def capture(page, actions, presets, *, evaluate):
        observed.update(actions=actions, presets=presets)
        return {"schema": "page_actions.v1", "state": "complete"}

    monkeypatch.setattr(page_actions, "run_actions", capture)
    monkeypatch.setattr(
        render,
        "_bounded_action_evaluator",
        lambda *_args: (lambda *_args, **_kwargs: None, None),
    )
    config = crawl_config.load(
        overrides={
            "rendering.mode": "js",
            "rendering.actions": [{"action": "wait_ms", "ms": 25}],
            "rendering.presets": ["expand_tabs"],
        }
    )["rendering"]

    result = render_document("https://example.com/", config)

    assert result["renderer"]["page_actions"]["state"] == "complete"
    assert observed["actions"] == [{"action": "wait_ms", "ms": 25}]
    assert observed["presets"] == ["expand_tabs"]


def test_no_screenshot_without_an_artifacts_dir(fake_stack):
    config = _rendering_config()
    config["artifacts"]["screenshots"] = True
    result = render_document("https://example.com/", config, artifacts_dir=None)
    assert result["screenshot_path"] is None
    assert fake_stack["page"].screenshot_calls == []


def test_persistent_profile_is_refused_until_pinned_cookie_continuity_is_proven(
    fake_stack, tmp_path
):
    config = _rendering_config(
        persistent_profile=True, persistent_profile_dir=str(tmp_path / "profile")
    )
    result = render_document("https://example.com/", config)
    assert result["ok"] is False
    assert "cookie continuity" in result["error"]
    assert not fake_stack["chromium"].launch_persistent_calls
    assert fake_stack["chromium"].launched is False


def test_without_a_persistent_profile_the_ordinary_launch_path_is_used(fake_stack):
    render_document("https://example.com/", _rendering_config())
    assert fake_stack["chromium"].launched is True
    assert fake_stack["chromium"].launch_calls == [{"chromium_sandbox": True}]
    assert fake_stack["chromium"].launch_persistent_calls == []


def test_explicit_local_chrome_executable_keeps_the_sandbox(fake_stack, monkeypatch, tmp_path):
    chrome = tmp_path / "chrome"
    chrome.write_text("synthetic executable")
    chrome.chmod(0o755)
    monkeypatch.setenv("SEOHEAD_CHROME", str(chrome))

    result = render_document("https://example.com/", _rendering_config())

    assert result["ok"] is True
    assert fake_stack["chromium"].launch_calls == [
        {"chromium_sandbox": True, "executable_path": str(chrome.resolve())}
    ]


def test_invalid_explicit_chrome_path_fails_without_fallback(fake_stack, monkeypatch, tmp_path):
    monkeypatch.setenv("SEOHEAD_CHROME", str(tmp_path / "missing-chrome"))

    result = render_document("https://example.com/", _rendering_config())

    assert result["ok"] is False
    assert "SEOHEAD_CHROME" in result["error"]
    assert fake_stack["chromium"].launched is False


def test_root_is_refused_before_any_playwright_call(fake_stack, monkeypatch):
    monkeypatch.setattr(os, "geteuid", lambda: 0, raising=False)
    result = render_document("https://example.com/", _rendering_config())
    assert result["ok"] is False
    assert "sandbox" in result["error"]
    assert fake_stack["chromium"].launched is False


def test_missing_playwright_is_reported_as_data(monkeypatch):
    import builtins

    real_import = builtins.__import__

    def fake(name, *a, **kw):
        if name.startswith("playwright"):
            raise ImportError("no playwright")
        return real_import(name, *a, **kw)

    monkeypatch.setattr(builtins, "__import__", fake)
    monkeypatch.setattr(socket, "getaddrinfo", _fixture_dns)
    result = render_document("https://example.com/", _rendering_config())
    assert result["ok"] is False
    assert "playwright install chromium" in result["install"]


def test_a_private_target_is_refused_before_launching_a_browser(fake_stack):
    result = render_document("http://127.0.0.1:9222/", _rendering_config())
    assert result["ok"] is False
    assert fake_stack["chromium"].launched is False


# ── the rendered page must be fetched as the same client as the raw crawl ────


class _VaryingPage(_FakePage):
    """A page whose body depends on who asked for it.

    Stands in for a server sending ``Vary: User-Agent`` — legal, common, and
    nothing to do with JavaScript. The context's options are read at
    ``content()`` time because that is when render_document has finished
    configuring the browser identity.
    """

    def __init__(self, context_options: dict, bodies: dict[str, str]):
        super().__init__()
        self._options = context_options
        self._bodies = bodies

    def content(self):
        seen = self._options.get("user_agent")
        return self._bodies.get(seen, self._bodies["__other__"])


def test_the_browser_identifies_as_the_crawl_did(fake_stack):
    """The fuller fetch replaces a page's body-derived evidence — title, word count,
    links — so it must ask the origin as the same client the static crawl used. Without
    an explicit User-Agent the context gets Chromium's own, which in headless mode
    advertises HeadlessChrome, and a report then mixes two populations: escalated pages
    described from what the server serves a headless browser, every other page from what
    it serves the toolkit. #199 fixed this for the single-page render_check() probe; this
    is the same confound at the crawl-wide call site.
    """
    from seohead.recon.net import UA

    render_document("https://example.com/", _rendering_config(), user_agent=UA)

    assert fake_stack["context"].options["user_agent"] == UA


def test_an_explicit_crawl_user_agent_reaches_the_browser(fake_stack):
    """``http.user_agent`` is a setting an operator sets deliberately — often because a
    site is allow-listed by it. A rendered fetch that ignores it is refused entry, or
    served something different, on exactly the sites where the setting was needed."""
    render_document("https://example.com/", _rendering_config(), user_agent="AcmeAudit/2.0")

    assert fake_stack["context"].options["user_agent"] == "AcmeAudit/2.0"


def test_a_varying_server_returns_the_crawls_own_body(monkeypatch):
    """End to end through the stub: the body render_document brings back is the one the
    origin serves the crawl's identity, not the one it serves an unrecognised client."""
    from seohead.recon.net import UA

    context_options: dict = {}
    page = _VaryingPage(
        context_options,
        {
            UA: "<html><body>the real page</body></html>",
            "__other__": "<html><body>are you a robot?</body></html>",
        },
    )
    context = _FakeContext(page)
    context.options = context_options
    browser = _FakeBrowser(context)
    chromium = _FakeChromium(browser, context)
    pw = _FakePlaywright(chromium)

    fake_playwright = types.ModuleType("playwright")
    fake_sync_api = types.ModuleType("playwright.sync_api")
    fake_sync_api.sync_playwright = lambda: pw
    fake_playwright.sync_api = fake_sync_api
    monkeypatch.setitem(sys.modules, "playwright", fake_playwright)
    monkeypatch.setitem(sys.modules, "playwright.sync_api", fake_sync_api)
    monkeypatch.setattr(socket, "getaddrinfo", _fixture_dns)

    result = render_document("https://example.com/", _rendering_config(), user_agent=UA)

    assert result["ok"] is True
    assert "the real page" in result["html"]


# ── engine selection, viewport pair, page concurrency (#744) ────────────────


@pytest.mark.parametrize("engine", ["chromium", "firefox", "webkit"])
def test_each_supported_engine_reaches_its_own_launcher(fake_engines, engine):
    result = render_document("https://example.com/", _rendering_config(engine=engine))

    assert result["ok"] is True
    assert result["renderer"]["engine"] == f"playwright-{engine}"
    for name, launcher in fake_engines["launchers"].items():
        assert launcher.launched is (name == engine), name
    assert fake_engines["launchers"][engine].launch_calls == [
        dict(render.BROWSER_ENGINES[engine]["launch_options"])
    ]


def test_the_default_engine_remains_chromium(fake_engines):
    result = render_document("https://example.com/", _rendering_config())

    assert result["ok"] is True
    assert result["renderer"]["engine"] == "playwright-chromium"
    assert fake_engines["launchers"]["chromium"].launched is True
    assert fake_engines["launchers"]["firefox"].launched is False
    assert fake_engines["launchers"]["webkit"].launched is False


def test_an_unknown_engine_is_refused_without_launching_anything(fake_engines):
    config = _rendering_config()
    config["browser"]["engine"] = "safari"  # cfg.load refuses this; render must too
    result = render_document("https://example.com/", config)

    assert result["ok"] is False
    assert "unsupported rendering engine" in result["error"]
    assert all(not launcher.launched for launcher in fake_engines["launchers"].values())


def test_an_engine_without_mobile_support_gets_no_emulation_options(fake_engines):
    result = render_document("https://example.com/", _rendering_config(engine="firefox"))

    assert result["ok"] is True
    assert "is_mobile" not in fake_engines["context"].options
    assert "has_touch" not in fake_engines["context"].options


def test_firefox_plus_mobile_emulation_is_a_capability_error(fake_engines):
    config = _rendering_config()
    config["browser"]["engine"] = "firefox"
    config["browser"]["mobile_emulation"] = True
    result = render_document("https://example.com/", config)

    assert result["ok"] is False
    assert "cannot emulate" in result["error"]
    assert "mobile_emulation" in result["error"]
    assert fake_engines["launchers"]["firefox"].launched is False


def test_firefox_plus_touch_emulation_reaches_the_context(fake_engines):
    """has_touch is a general context option Playwright accepts on every
    engine -- only is_mobile is documented unsupported on Firefox, so touch
    emulation alone must not be gated."""
    result = render_document(
        "https://example.com/",
        _rendering_config(engine="firefox", touch_emulation=True),
    )

    assert result["ok"] is True
    assert fake_engines["launchers"]["firefox"].launched is True
    assert fake_engines["context"].options["has_touch"] is True
    assert "is_mobile" not in fake_engines["context"].options


def test_a_missing_browser_binary_is_a_distinct_actionable_error(fake_engines):
    """Missing binaries are the commonest render failure; 'chromium not
    installed' must say which browser to install, not fall back quietly."""
    fake_engines["launchers"]["webkit"].fail = RuntimeError(
        "Executable doesn't exist at /browsers/webkit-1234\n"
        "Looks like Playwright Test or Playwright was just installed or updated."
    )
    result = render_document("https://example.com/", _rendering_config(engine="webkit"))

    assert result["ok"] is False
    assert "webkit" in result["error"]
    assert "not installed" in result["error"]
    assert result["install"] == "python -m playwright install webkit"
    assert fake_engines["launchers"]["chromium"].launched is False
    assert fake_engines["launchers"]["firefox"].launched is False


def test_a_launch_failure_keeps_requested_engine_provenance(fake_engines):
    fake_engines["launchers"]["firefox"].fail = RuntimeError("crash on start")
    result = render_document("https://example.com/", _rendering_config(engine="firefox"))

    assert result["ok"] is False
    assert result["renderer"]["engine"] == "playwright-firefox"
    assert result["renderer"]["engine_version"] == "unknown"
    assert fake_engines["launchers"]["chromium"].launched is False


@pytest.mark.parametrize(
    "preset,size",
    [("desktop", {"width": 1366, "height": 768}), ("mobile", {"width": 390, "height": 844})],
)
def test_named_viewport_presets_still_reach_the_context(fake_stack, preset, size):
    result = render_document("https://example.com/", _rendering_config(viewport=preset))

    assert result["ok"] is True
    assert fake_stack["context"].options["viewport"] == size
    assert result["renderer"]["settings"]["viewport"] == size


def test_a_custom_viewport_pair_reaches_the_context_and_record(fake_stack):
    result = render_document(
        "https://example.com/",
        _rendering_config(viewport_width=800, viewport_height=600),
    )

    assert result["ok"] is True
    assert fake_stack["context"].options["viewport"] == {"width": 800, "height": 600}
    assert result["renderer"]["settings"]["viewport"] == {"width": 800, "height": 600}


def test_a_custom_pair_overrides_the_named_preset_dimensions(fake_stack):
    """viewport='mobile' + 800x600 renders at 800x600, not the preset's
    390x844 -- but the pair carries dimensions only: mobile and touch
    emulation are separate settings that stay off unless asked for."""
    result = render_document(
        "https://example.com/",
        _rendering_config(viewport="mobile", viewport_width=800, viewport_height=600),
    )

    assert fake_stack["context"].options["viewport"] == {"width": 800, "height": 600}
    assert "is_mobile" not in fake_stack["context"].options
    assert "has_touch" not in fake_stack["context"].options
    assert result["renderer"]["settings"]["viewport"] == {"width": 800, "height": 600}


def test_a_custom_pair_keeps_explicitly_enabled_emulation_flags(fake_stack):
    """The preset stops supplying dimensions, never the emulation flags an
    operator set: mobile + touch stay on when the pair overrides 390x844."""
    render_document(
        "https://example.com/",
        _rendering_config(
            viewport="mobile",
            viewport_width=800,
            viewport_height=600,
            mobile_emulation=True,
            touch_emulation=True,
        ),
    )

    options = fake_stack["context"].options
    assert options["viewport"] == {"width": 800, "height": 600}
    assert options["is_mobile"] is True
    assert options["has_touch"] is True


def test_a_partial_viewport_pair_fails_before_any_browser_starts(fake_stack):
    config = _rendering_config()
    config["browser"]["viewport_width"] = 800  # cfg.load refuses this too; render must not guess
    result = render_document("https://example.com/", config)

    assert result["ok"] is False
    assert "complete pair" in result["error"]
    assert fake_stack["chromium"].launched is False


def test_an_out_of_range_viewport_fails_before_any_browser_starts(fake_stack):
    config = _rendering_config(viewport_width=999999, viewport_height=10)
    result = render_document("https://example.com/", config)

    assert result["ok"] is False
    assert "complete pair" in result["error"]
    assert fake_stack["chromium"].launched is False


def test_a_failed_render_still_records_requested_engine_and_viewport(fake_stack):
    """Timeout/cancellation provenance must say what was attempted -- an
    800px webkit failure must not read like a desktop chromium success."""
    fake_stack["page"].goto = lambda *a, **k: (_ for _ in ()).throw(
        RuntimeError("Navigation timeout")
    )
    config = _rendering_config(viewport_width=800, viewport_height=600)
    config["browser"]["engine"] = "webkit"
    result = render_document("https://example.com/", config)

    assert result["ok"] is False
    assert result["renderer"]["engine"] == "playwright-webkit"
    assert result["renderer"]["settings"]["viewport"] == {"width": 800, "height": 600}
    assert result["renderer"]["page_concurrency"] == 1


def test_the_configured_page_concurrency_is_recorded(fake_stack):
    result = render_document("https://example.com/", _rendering_config(page_concurrency=4))

    assert result["ok"] is True
    assert result["renderer"]["page_concurrency"] == 4


@pytest.mark.parametrize("engine", ["chromium", "firefox", "webkit"])
def test_request_security_controls_hold_for_every_engine(fake_engines, engine):
    """Engine selection changes the launcher, never the guards: the pinned
    route, WebSocket interception, and service-worker blocking are contract,
    not Chromium behaviour."""
    result = render_document("https://example.com/", _rendering_config(engine=engine))

    assert result["ok"] is True
    context = fake_engines["context"]
    assert context.options["service_workers"] == "block"
    assert context.routes and context.routes[0][0] == "**/*"
    assert context.ws_routes and context.ws_routes[0][0] == "**/*"


def test_missing_playwright_names_the_selected_engine(monkeypatch):
    import builtins

    real_import = builtins.__import__

    def _no_playwright(name, *args, **kwargs):
        if name.startswith("playwright"):
            raise ImportError("No module named 'playwright'")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", _no_playwright)
    monkeypatch.setattr(socket, "getaddrinfo", _fixture_dns)
    result = render_document("https://example.com/", _rendering_config(engine="webkit"))

    assert result["ok"] is False
    assert result["error"] == "Playwright is required"
    assert "playwright install webkit" in result["install"]
